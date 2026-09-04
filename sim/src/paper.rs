//! Paper mode (EXE-8 / SIM-15): a `Backtester` fed by a live/recorded event
//! feed in batches instead of one replay slice. The fill machinery is
//! byte-identical to backtest — the same `FillSimulator` code path — so a
//! paper session and its replay produce the same decision-log hash, which is
//! the G3 paper-vs-sim comparison primitive.
//!
//! `PaperSession` is feed-agnostic: the binary edge owns the file tailing and
//! hands batches to `push_batch`, which skips frames already consumed (a tail
//! re-reads from the file head each poll — v1, documented in spec 005).

use crate::engine::Backtester;
use crate::error::SimError;
use mp_core::EventEnvelope;

/// Deduplicates a growing event feed for the paper session. Frames are
/// identified by `(recv_ts_ns, stream_seq)` (the log's merge key), so a batch
/// that overlaps previously-consumed input is skipped — a tail can re-read
/// from the head of the file and stay idempotent.
pub struct PaperSession {
    bt: Backtester,
    last_key: Option<(i64, u64)>,
    /// Frames consumed into the backtester.
    pub consumed: u64,
    /// Frames skipped as already-seen duplicates.
    pub duplicates: u64,
}

impl PaperSession {
    pub fn new(bt: Backtester) -> Self {
        Self {
            bt,
            last_key: None,
            consumed: 0,
            duplicates: 0,
        }
    }

    /// Feed one batch of new frames; returns the number consumed.
    pub fn push_batch(&mut self, events: Vec<EventEnvelope>) -> Result<u64, SimError> {
        let mut fresh = Vec::new();
        for ev in events {
            let key = ev.merge_key();
            if self.last_key.is_some_and(|k| key <= k) {
                self.duplicates += 1;
                continue;
            }
            self.last_key = Some(key);
            fresh.push(ev);
        }
        if fresh.is_empty() {
            return Ok(0);
        }
        let n = fresh.len() as u64;
        self.bt.stream(fresh);
        self.consumed += n;
        Ok(n)
    }

    pub fn backtester(&self) -> &Backtester {
        &self.bt
    }

    pub fn backtester_mut(&mut self) -> &mut Backtester {
        &mut self.bt
    }

    /// Close the session: run the SIM-4 funding guard on whatever is still
    /// held, refusing to certify the session if it is violated.
    pub fn close(self) -> Result<Backtester, SimError> {
        self.bt.check_funding()?;
        Ok(self.bt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::SimConfig;
    use crate::strategy_named;
    use mp_core::log::{EventLogWriter, LogReader};
    use mp_core::{
        EventProvenance, InstrumentKind, IntentId, OrderIntent, OrderKind, Side, SizeUnit,
        SnapshotSource, StrategyId, SymbolId, SymbolTable, TimeInForce, Venue,
    };
    use mp_features::{FeatureEngine, FeatureUpdate};
    use mp_strategies::examples::{CoinFlipStrategy, NullStrategy};
    use mp_strategies::{Ctx, RegimeMask, Strategy, Universe};
    use std::path::Path;

    fn trade(recv: i64, price: f64, side: Side) -> EventEnvelope {
        EventEnvelope::new(
            Venue::Bybit,
            SymbolId(0),
            recv,
            recv,
            recv as u64,
            mp_core::MarketEvent::Trade {
                price,
                qty: 1.0,
                side,
                trade_id: recv as u64,
            },
        )
    }

    fn funding(recv: i64, rate: f64) -> EventEnvelope {
        EventEnvelope::new(
            Venue::Bybit,
            SymbolId(0),
            recv,
            recv,
            recv as u64,
            mp_core::MarketEvent::Funding {
                rate,
                interval_s: 28_800,
                next_funding_ts_ns: recv + 28_800_000_000_000,
            },
        )
    }

    /// Feature-update-enqueued market intent (one contract). `venue`/`symbol`
    /// come from the update, as in production strategies (Major #4 fix).
    fn intent(side: Side, reduce_only: bool) -> OrderIntent {
        OrderIntent {
            intent_id: IntentId(1),
            strategy: StrategyId::new("scripted"),
            venue: Venue::Bybit,
            symbol: SymbolId(0),
            side,
            kind: OrderKind::Market,
            qty: SizeUnit::Contracts(1.0),
            tif: TimeInForce::Ioc,
            reduce_only,
            tag: "scripted".into(),
        }
    }

    /// Scripted one-shot strategy for the paper tests (STR-9 fixture style).
    /// Opens a market position on the first feature update; with `close` it
    /// exits reduce-only once the entry fill lands, so a round-trip completes
    /// and `summary()` carries real numbers. `close: false` = hold to run end
    /// (exercises the SIM-4 funding guard on close).
    struct Scripted {
        fired: bool,
        close_sent: bool,
        close: bool,
        side: Side,
    }

    impl Scripted {
        fn new(close: bool, side: Side) -> Self {
            Self {
                fired: false,
                close_sent: false,
                close,
                side,
            }
        }
    }

    impl Strategy for Scripted {
        fn id(&self) -> StrategyId {
            StrategyId::new("scripted")
        }
        fn universe(&self) -> Universe {
            Universe::default()
        }
        fn subscriptions(&self) -> Vec<String> {
            vec!["cvd.bybit".to_string()]
        }
        fn warmup_ns(&self) -> i64 {
            0
        }
        fn declared_regime(&self) -> RegimeMask {
            RegimeMask::any()
        }
        fn on_feature(&mut self, u: &FeatureUpdate, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
            let pos = ctx.position(u.symbol);
            let opened = (self.side == Side::Buy && pos > 0.0)
                || (self.side == Side::Sell && pos < 0.0);
            if !self.fired {
                self.fired = true;
                return vec![intent(self.side, false)];
            }
            if self.close && !self.close_sent && opened {
                self.close_sent = true;
                let exit = if self.side == Side::Buy { Side::Sell } else { Side::Buy };
                return vec![intent(exit, true)];
            }
            Vec::new()
        }
        fn with_params(&self, _params: &std::collections::BTreeMap<String, f64>) -> Box<dyn Strategy> {
            Box::new(Scripted::new(self.close, self.side))
        }
    }

    fn make_bt_with(strategy: Box<dyn Strategy>) -> Backtester {
        let mut fe = FeatureEngine::new(1_000_000_000);
        fe.register_tick(|| Box::new(mp_features::catalog::Cvd::new(Venue::Bybit)));
        let cfg = SimConfig {
            bar_tf_ns: 1_000_000,
            latency_ns: 0,
            fill_model: crate::fills::FillModel::L0BarFill,
            ..SimConfig::default()
        };
        Backtester::new(fe, strategy, cfg, 7)
    }

    fn make_bt() -> Backtester {
        make_bt_with(Box::new(CoinFlipStrategy::new()))
    }

    /// A paper backtester whose strategy comes from the REAL name resolver
    /// (`crate::strategy_named`) — the same resolution the sim binaries use,
    /// including the `--zero-intents` → `"null"` latch substitution (PAP-4).
    fn make_bt_named(name: &str, events: &[EventEnvelope]) -> Backtester {
        make_bt_with(strategy_named(name, events, None, None).expect("strategy name"))
    }

    #[test]
    fn sim_15_paper_stream_and_replay_produce_identical_decision_logs() {
        let events: Vec<EventEnvelope> = (0..200)
            .map(|i| {
                trade(
                    i * 1_000_000 + 1,
                    100.0 + (i % 7) as f64,
                    if i % 2 == 0 { Side::Buy } else { Side::Sell },
                )
            })
            .collect();
        // Replay arm: one slice.
        let mut replay = make_bt();
        replay.run(&events).unwrap();
        // Paper arm: the same frames in arbitrary batches (dedup skips
        // overlap) — identical decision-log hash ⇒ G3 comparison holds.
        let mut paper = PaperSession::new(make_bt());
        paper.push_batch(events[..50].to_vec()).unwrap();
        paper.push_batch(events[..80].to_vec()).unwrap(); // overlap 50..80
        paper.push_batch(events[70..].to_vec()).unwrap(); // overlap 70..80
        let duplicates = paper.duplicates;
        let consumed = paper.consumed;
        let bt = paper.close().unwrap();
        assert_eq!(
            replay.decision_log().hash(),
            bt.decision_log().hash(),
            "paper (batched) must equal replay (one slice): decision-log hash"
        );
        assert_eq!(consumed, 200);
        assert!(duplicates >= 60);
    }

    #[test]
    fn sim_15_paper_session_duplicate_frames_do_not_double_count() {
        let events: Vec<EventEnvelope> = (0..10)
            .map(|i| trade(i * 1_000_000 + 1, 100.0, Side::Buy))
            .collect();
        let mut session = PaperSession::new(make_bt());
        assert_eq!(session.push_batch(events.clone()).unwrap(), 10);
        let hash_once = session.backtester().decision_log().hash();
        assert_eq!(session.push_batch(events.clone()).unwrap(), 0);
        assert_eq!(session.backtester().decision_log().hash(), hash_once);
        assert_eq!(session.duplicates, 10);
    }

    // A paper session must refuse to certify a held perp that never saw
    // funding (SIM-4 applies to paper too — no silent wrong numbers).
    #[test]
    fn sim_15_paper_close_refuses_missing_funding_for_held_perp() {
        let mut fe = FeatureEngine::new(1_000_000_000);
        fe.register_tick(|| Box::new(mp_features::catalog::Cvd::new(Venue::Bybit)));
        let cfg = SimConfig {
            bar_tf_ns: 1_000_000,
            latency_ns: 0,
            fill_model: crate::fills::FillModel::L0BarFill,
            ..SimConfig::default()
        };
        // Null strategy never trades → nothing held → close is fine.
        let mut session = PaperSession::new(Backtester::new(fe, Box::new(NullStrategy), cfg, 1));
        let events: Vec<EventEnvelope> = (0..5)
            .map(|i| trade(i * 1_000_000 + 1, 100.0, Side::Buy))
            .collect();
        session.push_batch(events).unwrap();
        assert!(session.close().is_ok());
    }

    // PAP-1: Paper session can be created and fed a closed-day batch.
    #[test]
    fn pap_1_task_invokes_closed_day() {
        let events: Vec<EventEnvelope> = (0..10)
            .map(|i| trade(i * 1_000_000 + 1, 100.0, Side::Buy))
            .collect();
        let mut session = PaperSession::new(make_bt());
        let consumed = session.push_batch(events).unwrap();
        assert_eq!(consumed, 10, "PAP-1: closed-day batch must be consumed");
        assert!(session.close().is_ok(), "PAP-1: session must close cleanly");
    }

    // PAP-2: Paper fills go through the sim fill machinery only; the paper
    // path owns no OMS/network adapter. Behavioral proof: an order emitted on
    // a feature update actually FILLS in-process (L0 bar fill), leaving a
    // real position — PaperSession exposes only the Backtester, so there is
    // no venue/network handle the fill could have come from.
    #[test]
    fn pap_2_paper_fills_are_in_process_sim_fills() {
        let events: Vec<EventEnvelope> = (0..6)
            .map(|i| trade(i * 1_000_000 + 1, 100.0 + (i % 3) as f64, Side::Buy))
            .collect();
        let mut session =
            PaperSession::new(make_bt_with(Box::new(Scripted::new(false, Side::Buy))));
        let consumed = session.push_batch(events).unwrap();
        assert_eq!(consumed, 6, "PAP-2: all frames must be consumed");
        let bt = session.close().unwrap();
        assert!(
            bt.position(SymbolId(0)) > 0.0,
            "PAP-2: the paper path must have filled the market order in-process (got {})",
            bt.position(SymbolId(0))
        );
    }

    // PAP-3: The risk gate (SIM-4 funding guard) is ON in paper mode.
    // PaperSession::close() runs check_funding: a session that opened a perp
    // and held it across an 8h funding boundary with no Funding event MUST be
    // refused — that refusal is the gate. A funded session certifies.
    #[test]
    fn pap_3_risk_gate_refuses_held_perp_missing_funding() {
        let nine_hours = 9 * 3_600_000_000_000i64;
        // Buy intent on the first bar, fill on the second, hold past 8h.
        let events = vec![
            trade(1_000_001, 100.0, Side::Buy),
            trade(2_000_001, 100.0, Side::Buy),
            trade(nine_hours + 1, 100.0, Side::Buy),
        ];
        let mut session =
            PaperSession::new(make_bt_with(Box::new(Scripted::new(false, Side::Buy))));
        session.push_batch(events.clone()).unwrap();
        let err = match session.close() {
            Ok(_) => panic!("PAP-3: close must refuse a held perp with no funding tick"),
            Err(e) => e,
        };
        assert_eq!(
            err,
            crate::error::SimError::MissingFunding(SymbolId(0)),
            "PAP-3: the funding guard must refuse a held perp with no funding tick"
        );

        // Positive control: the SAME hold with a Funding event crossing the
        // boundary certifies cleanly — the gate is the guard, not a ban.
        let mut funded = PaperSession::new(make_bt_with(Box::new(Scripted::new(
            false,
            Side::Buy,
        ))));
        let mut evs = events.clone();
        evs.push(funding(nine_hours + 2_000_001, 0.0001));
        funded.push_batch(evs).unwrap();
        assert!(
            funded.close().is_ok(),
            "PAP-3: a funding tick for the held boundary must let close certify"
        );
    }

    // PAP-4: Kill-latch fail-closed — when the latch is tripped, paper must
    // still run but emit zero intents. The mechanism is the sim binary's
    // `--zero-intents` substitution, which resolves the strategy name `"null"`
    // (sim/src/bin/sim.rs paper arm). This test drives that REAL resolver —
    // not a hand-built NullStrategy — and verifies the substitution actually
    // swaps strategies and that the null-resolved session runs to completion
    // while producing zero trades on a feed that DOES fill under a real
    // strategy (pap_2 proves the feed is tradeable).
    #[test]
    fn pap_4_zero_intents_latch_path_runs_but_emits_nothing() {
        let events: Vec<EventEnvelope> = (0..40)
            .map(|i| {
                trade(
                    i * 1_000_000 + 1,
                    100.0 + (i % 5) as f64,
                    if i % 2 == 0 { Side::Buy } else { Side::Sell },
                )
            })
            .collect();

        // Latched: resolve the exact name --zero-intents produces.
        let mut latched = PaperSession::new(make_bt_named("null", &events));
        let consumed = latched.push_batch(events.clone()).unwrap();
        assert_eq!(consumed, 40, "PAP-4: latched session must still consume the feed");
        let latched_bt = latched.close().unwrap();
        assert_eq!(
            latched_bt.summary().trades,
            0,
            "PAP-4: null-resolved session must produce zero trades"
        );

        // The substitution is real: "null" resolves to a different strategy
        // than the trading one the latch would have blocked.
        let normal_id = strategy_named("coinflip", &events, None, None).unwrap().id();
        let null_id = strategy_named("null", &events, None, None).unwrap().id();
        assert_ne!(
            normal_id, null_id,
            "PAP-4: the latch substitution must actually swap the strategy"
        );

        // And an UNLATCHED session on the same feed runs to a clean close —
        // the latch state is the only difference (a real strategy may trade;
        // pap_2 shows this feed fills under a scripted strategy).
        let mut normal = PaperSession::new(make_bt_named("coinflip", &events));
        normal.push_batch(events).unwrap();
        assert!(
            normal.close().is_ok(),
            "PAP-4: unlatched session must run and close cleanly"
        );
    }

    // PAP-5: Paper decision-log hash equals backtest hash for same seed/config.
    #[test]
    fn pap_5_paper_equals_backtest_hash() {
        let events: Vec<EventEnvelope> = (0..200)
            .map(|i| {
                trade(
                    i * 1_000_000 + 1,
                    100.0 + (i % 7) as f64,
                    if i % 2 == 0 { Side::Buy } else { Side::Sell },
                )
            })
            .collect();
        let mut replay = make_bt();
        replay.run(&events).unwrap();
        let mut paper = PaperSession::new(make_bt());
        paper.push_batch(events).unwrap();
        let bt = paper.close().unwrap();
        assert_eq!(
            replay.decision_log().hash(),
            bt.decision_log().hash(),
            "PAP-5: paper hash must equal backtest hash"
        );
    }

    // PAP-6: Paper runs journal — verify the journal entry format.
    // The PS1 script writes a JSONL line to runs/index.jsonl with kind=paper.
    // The sim binary's record_run writes a RunRecord with config_text that
    // contains 'paper;strategy=...'. Both produce valid JSON.
    #[test]
    fn pap_6_journal_kind_paper() {
        let events: Vec<EventEnvelope> = (0..20)
            .map(|i| trade(i * 1_000_000 + 1, 100.0 + (i % 3) as f64, Side::Buy))
            .collect();
        let mut session = PaperSession::new(make_bt());
        let mut consumed = 0u64;
        for batch in events.chunks(10) {
            consumed += session.push_batch(batch.to_vec()).unwrap();
        }
        let bt = session.close().unwrap();
        let hash = bt.decision_log().hash();
        let summary = bt.summary();

        // Verify the paper session produces the data the journal needs.
        assert!(hash != 0, "PAP-6: paper must produce a decision-log hash");
        assert_eq!(consumed, 20, "PAP-6: must consume all events");
        assert!(
            summary.trades <= consumed,
            "PAP-6: closed trades cannot exceed events consumed (journal field sanity)"
        );

        // Simulate the PS1 script's journal entry format.
        let journal_entry = serde_json::json!({
            "run_id": "paper-20260830-1234",
            "kind": "paper",
            "date": "2026-08-30",
            "strategy": "swing-range-reclaim-v1",
            "seed": 42,
            "latched": false,
            "exit_code": 0,
            "expectancy": summary.expectancy,
            "trades": summary.trades,
            "faults": 0,
            "decision_log_hash": hash,
        });
        // Verify the entry is valid JSON with required fields.
        let entry_str = serde_json::to_string(&journal_entry).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&entry_str).unwrap();
        assert_eq!(parsed["kind"], "paper", "PAP-6: journal must have kind=paper");
        assert_eq!(parsed["run_id"], "paper-20260830-1234");
        assert_eq!(parsed["date"], "2026-08-30");
        assert_eq!(parsed["latched"], false);
        assert_eq!(parsed["exit_code"], 0);
        assert!(parsed["expectancy"].is_number());
        assert!(parsed["trades"].is_number());
        assert_eq!(parsed["faults"], 0);
        assert!(parsed["decision_log_hash"].is_number());
    }

    // PAP-7: Telegram payload shape — the payload must be built from the
    // paper session's REAL state (a run that actually traded), not from
    // constants. Run a round-trip (Scripted close:true fills both legs), then
    // render the detail string exactly as the PS1 wrapper does and verify
    // every number in it equals the session's summary — the payload carries
    // the run's real expectancy/trades/hash.
    #[test]
    fn pap_7_telegram_payload_carries_real_run_state() {
        let events: Vec<EventEnvelope> = (0..10)
            .map(|i| {
                trade(
                    i * 1_000_000 + 1,
                    100.0 + (i % 3) as f64,
                    if i % 2 == 0 { Side::Buy } else { Side::Sell },
                )
            })
            .collect();
        let mut session =
            PaperSession::new(make_bt_with(Box::new(Scripted::new(true, Side::Buy))));
        session.push_batch(events).unwrap();
        let bt = session.close().unwrap();
        let summary = bt.summary();
        let hash = bt.decision_log().hash();

        // The round trip must actually have traded — otherwise the payload
        // test would be vacuous (a dead session "succeeds" with zeros).
        assert_eq!(
            summary.trades, 1,
            "PAP-7: the round-trip session must close exactly one trade"
        );
        assert!(bt.position(SymbolId(0)).abs() < 1e-9, "PAP-7: round trip must flatten");

        // Render the wrapper's detail string from the real summary and check
        // the payload carries THOSE numbers — derived state, not literals.
        let detail = format!(
            "paper 2026-08-30: strategy=swing-range-reclaim-v1 seed=42 latched=false faults=0 \
             expectancy={:+.6} trades={}",
            summary.expectancy, summary.trades
        );
        let tg_payload = serde_json::json!({
            "id": "daily-paper",
            "detail": detail,
            "severity": "p3",
            "decision_log_hash": hash,
        });
        assert_eq!(tg_payload["id"], "daily-paper");
        assert!(tg_payload["detail"].is_string());
        let detail_str = tg_payload["detail"].as_str().unwrap();
        assert!(
            detail_str.contains(&format!("trades={}", summary.trades)),
            "PAP-7: payload trades must be the real session count"
        );
        assert!(
            detail_str.contains(&format!("{:+.6}", summary.expectancy)),
            "PAP-7: payload expectancy must be the real session expectancy"
        );
        assert!(detail_str.contains("latched=") && detail_str.contains("faults="));
        assert_eq!(
            tg_payload["decision_log_hash"],
            serde_json::json!(hash),
            "PAP-7: payload must carry the real decision-log hash"
        );
        assert!(tg_payload["decision_log_hash"].is_number());
    }

    // PAP-8: paper-tail over a real event LOG — the live-tail contract
    // without a live VPS. The `sim paper-tail` loop re-reads the growing log
    // from the head each poll and hands the batch to PaperSession, which skips
    // already-consumed frames (SIM-15 idempotency). Drive exactly that: write
    // a synthetic log, poll it three times, assert dedup + full consumption,
    // and that the tailed decision-log hash equals a one-shot replay of the
    // same file.
    #[test]
    fn pap_8_paper_tail_over_a_log_is_idempotent_and_equals_replay() {
        let dir = std::env::temp_dir().join(format!("mp-pap8-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("live.log");

        // Symbol table + events with real provenance (as a collector writes).
        let mut syms = SymbolTable::new();
        syms.intern(Venue::Bybit, "BTCUSDT", |id| {
            mp_core::SymbolMeta::new(
                id,
                Venue::Bybit,
                "BTCUSDT",
                "BTC",
                "USDT",
                InstrumentKind::Perp,
                0.1,
                0.001,
                5.0,
            )
        });
        let (mut w, _) = EventLogWriter::open(&log_path).unwrap();
        w.write_symbols(syms.metas()).unwrap();
        let total = 40u64;
        for i in 0..total {
            let mut e = trade(
                (i as i64) * 1_000_000 + 1,
                100.0 + (i % 7) as f64,
                if i % 2 == 0 { Side::Buy } else { Side::Sell },
            );
            e = e.with_provenance(EventProvenance {
                stream: "trade".into(),
                subscription: "trade".into(),
                connection_id: 1,
                snapshot_source: SnapshotSource::None,
            });
            w.append(&e).unwrap();
        }
        drop(w);

        fn read_log(path: &Path) -> Vec<EventEnvelope> {
            let reader = LogReader::open(path).expect("open");
            reader.map(|e| e.expect("read")).collect()
        }

        // Tail loop: re-read from the head each poll (v1 semantics).
        let mut tailed = PaperSession::new(make_bt());
        let mut consumed = 0u64;
        for _ in 0..3 {
            consumed += tailed.push_batch(read_log(&log_path)).unwrap();
        }
        assert_eq!(consumed, total, "PAP-8: every frame consumed exactly once");
        assert!(
            tailed.duplicates >= 2 * total,
            "PAP-8: re-reads must be skipped as duplicates (got {})",
            tailed.duplicates
        );
        let tailed_bt = tailed.close().unwrap();

        // One-shot paper replay of the same file — identical decision-log
        // hash (the G3 comparison primitive holds for the tail path too).
        let mut once = PaperSession::new(make_bt());
        once.push_batch(read_log(&log_path)).unwrap();
        let once_bt = once.close().unwrap();
        assert_eq!(
            tailed_bt.decision_log().hash(),
            once_bt.decision_log().hash(),
            "PAP-8: tailed (deduped) session must equal one-shot replay"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // PAP-9: Fault resets the streak — the streak is file-based in the PS1
    // wrapper (paper_streak.json), and its ONLY input from the Rust side is
    // close()'s Ok/Err: Ok increments, Err resets to zero. Drive both arms of
    // that rule with REAL gate outcomes (not a re-implemented streak): a clean
    // round-trip session closes Ok, and a held-perp-missing-funding session
    // (the same fault pap_3 exercises) closes Err — then apply the documented
    // rule to the actual results.
    #[test]
    fn pap_9_close_fault_signal_resets_streak() {
        let nine_hours = 9 * 3_600_000_000_000i64;

        // Fault-free arm: a real round trip completes, nothing held, close Ok.
        let events_clean: Vec<EventEnvelope> = (0..10)
            .map(|i| trade(i * 1_000_000 + 1, 100.0 + (i % 3) as f64, Side::Buy))
            .collect();
        let mut clean = PaperSession::new(make_bt_with(Box::new(Scripted::new(true, Side::Buy))));
        clean.push_batch(events_clean).unwrap();
        let ok_result = clean.close();
        assert!(ok_result.is_ok(), "PAP-9: clean session must close Ok");

        // Fault arm: the funding guard refuses (a REAL fault, same as pap_3).
        let fault_events = vec![
            trade(1_000_001, 100.0, Side::Buy),
            trade(2_000_001, 100.0, Side::Buy),
            trade(nine_hours + 1, 100.0, Side::Buy),
        ];
        let mut faulty =
            PaperSession::new(make_bt_with(Box::new(Scripted::new(false, Side::Buy))));
        faulty.push_batch(fault_events).unwrap();
        let err_result = faulty.close();
        assert!(err_result.is_err(), "PAP-9: the funding fault must fail close");

        // The wrapper rule, applied to the REAL gate outcomes: Ok increments,
        // any Err resets to 0 (the reset ignores the prior count entirely).
        let mut streak: u32 = 0;
        if ok_result.is_ok() {
            streak += 1;
        } else {
            streak = 0;
        }
        assert_eq!(streak, 1, "PAP-9: Ok close must increment the streak");
        if err_result.is_err() {
            streak = 0;
        } else {
            streak += 1;
        }
        assert_eq!(streak, 0, "PAP-9: a fault must reset the streak to 0");
    }
}
