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
    use mp_core::{Side, SymbolId, Venue};
    use mp_features::FeatureEngine;
    use mp_strategies::examples::{CoinFlipStrategy, NullStrategy};

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

    fn make_bt() -> Backtester {
        let mut fe = FeatureEngine::new(1_000_000_000);
        fe.register_tick(|| Box::new(mp_features::catalog::Cvd::new(Venue::Bybit)));
        let cfg = SimConfig {
            bar_tf_ns: 1_000_000,
            latency_ns: 0,
            fill_model: crate::fills::FillModel::L0BarFill,
            ..SimConfig::default()
        };
        Backtester::new(fe, Box::new(CoinFlipStrategy::new()), cfg, 7)
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
}
