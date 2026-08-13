//! Day-level decision determinism check (spec 018 MOD-9..11).
//!
//! Replays a recorded session through the PRODUCTION runtime — feature engine
//! (`engine_from_config`, the SAME registration a live runner and the
//! materializer use, FEA-4) → strategy → risk — and proves the decision path
//! is reproducible:
//!
//! - **self-determinism (MOD-10)** — two fresh runs over the same recorded
//!   events must produce byte-identical decision logs;
//! - **live identity (MOD-9)** — when a live/paper decision-log summary
//!   exists for the day, the replay must match it (hash + counts; the rolling
//!   FNV-1a hash is the CONV-12 golden-hash byte-identity proof).
//!
//! `passed` requires both (live identity only when a live log exists). The
//! verdict is evidence the daily pipeline turns into a per-day artifact
//! (`data/scorecards/{date}.determinism.json`) that the promotion gate reads
//! (MOD-9: a diff blocks promotion).
//!
//! This module is storage-free: it takes `&[EventEnvelope]` + the universe;
//! the binary loads logs and writes artifacts (via mp-storage) at the edge.

use crate::engine::{Backtester, SimConfig};
use crate::fills::FillModel;
use mp_core::{EventEnvelope, StrategyId, SymbolId, Venue};
use mp_features::{engine_from_config, FeaturesConfig};
use mp_strategies::{CarryConfig, CarryV1, NullStrategy, Strategy, Universe};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The check's pinned seed (0xC0FFEE). Pinned so live-vs-replay comparison is
/// apples-to-apples; overridden via the config file.
pub const DEFAULT_SEED: u64 = 0xC0FFEE;

fn default_strategy() -> String {
    "carry-v1".to_string()
}
fn default_seed() -> u64 {
    DEFAULT_SEED
}

/// The replay's pinned runtime config (TOML, deny_unknown_fields — a typo'd
/// key is an error, not a silent default, CONV-16).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeterminismConfig {
    /// Strategy the replay runs: `carry-v1` (real funding strategy — the
    /// strongest proof) or `null` (pipeline-only watcher: feature/decision
    /// determinism over the event stream, no strategy state).
    #[serde(default = "default_strategy")]
    pub strategy: String,
    #[serde(default = "default_seed")]
    pub seed: u64,
    /// Optional per-strategy params (passed via `Strategy::with_params`).
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
}

impl DeterminismConfig {
    pub fn defaults() -> Self {
        Self {
            strategy: default_strategy(),
            seed: default_seed(),
            params: BTreeMap::new(),
        }
    }
    pub fn from_toml(s: &str) -> Result<Self, String> {
        toml::from_str(s).map_err(|e| format!("determinism config parse: {e}"))
    }
}

/// The SimConfig the check pins — the same paper/replay shape the `sim`
/// CLI's `paper`/`backtest` use (L0 bar fills, zero latency), so a future
/// live paper session's log is directly comparable.
fn check_sim_config() -> SimConfig {
    SimConfig {
        min_coverage: 1.0,
        bar_tf_ns: 1_000_000,
        latency_ns: 0,
        fill_model: FillModel::L0BarFill,
        ..SimConfig::default()
    }
}

/// Compact, serializable summary of one replay — the form the daily artifact
/// stores and a live/paper session records (`--live-log` input).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplaySummary {
    pub date: String,
    pub strategy: String,
    pub seed: u64,
    pub event_count: usize,
    /// Decision-log line count (byte-identity dimension).
    pub lines: usize,
    pub intents: u64,
    pub fills: u64,
    /// Rolling FNV-1a hash over the whole decision log (CONV-12).
    pub hash: u64,
}

/// Full in-process replay result — carries the lines so two runs can be
/// compared byte-for-byte before collapsing to a summary.
pub struct DayReplay {
    pub summary: ReplaySummary,
    pub lines: Vec<String>,
}

/// The check's verdict. `passed` = self-consistent AND (no live log OR replay
/// matches it). `divergence_line` names the first differing decision when any
/// comparison fails (the `why` for the runbook).
#[derive(Debug, Clone)]
pub struct DeterminismVerdict {
    pub date: String,
    pub strategy: String,
    pub seed: u64,
    pub event_count: usize,
    /// Two fresh runs over the same events produced byte-identical logs.
    pub self_consistent: bool,
    pub replayed_hash: u64,
    pub replayed_lines: usize,
    /// Whether a live/paper decision-log summary existed to compare against.
    pub live_present: bool,
    /// Replay vs live identity (None when no live log existed).
    pub live_matches: Option<bool>,
    /// First diverging decision line index (any failing comparison).
    pub divergence_line: Option<usize>,
    pub passed: bool,
    pub reason: String,
}

/// Instantiate the configured strategy over the day's universe. `null` is the
/// pipeline watcher; `carry-v1` the real funding strategy (spec 015).
pub fn strategy_for(
    cfg: &DeterminismConfig,
    venues: &[Venue],
    symbols: &[SymbolId],
) -> Result<Box<dyn Strategy>, String> {
    match cfg.strategy.as_str() {
        "null" => Ok(Box::new(NullStrategy)),
        "carry-v1" => {
            let s = CarryV1::new(
                StrategyId::new("carry-v1"),
                Universe {
                    venues: venues.to_vec(),
                    symbols: symbols.to_vec(),
                },
                CarryConfig::default(),
            );
            Ok(s.with_params(&cfg.params))
        }
        other => Err(format!(
            "unknown determinism strategy '{other}' (carry-v1|null)"
        )),
    }
}

/// Run the engine once over `events` (streaming — the same code shape a paper
/// session uses) and return the full replay.
pub fn replay(
    date: &str,
    events: &[EventEnvelope],
    venues: &[Venue],
    symbols: &[SymbolId],
    cfg: &DeterminismConfig,
    fe_cfg: &FeaturesConfig,
) -> Result<DayReplay, String> {
    let fe = engine_from_config(fe_cfg).map_err(|e| format!("feature engine: {e}"))?;
    let strat = strategy_for(cfg, venues, symbols)?;
    let mut bt = Backtester::from_strategies(fe, vec![strat], check_sim_config(), cfg.seed);
    // stream() (not run_checked): the check measures decision determinism of
    // whatever the day produced — it is not certifying a tradable run, so the
    // SIM-4 funding / SIM-6 coverage run-end guards do not apply.
    bt.stream(events.iter().cloned());
    let log = bt.decision_log();
    let lines = log.lines().to_vec();
    let summary = ReplaySummary {
        date: date.to_string(),
        strategy: cfg.strategy.clone(),
        seed: cfg.seed,
        event_count: events.len(),
        lines: lines.len(),
        intents: log.intent_count(),
        fills: log.fill_count(),
        hash: log.hash(),
    };
    Ok(DayReplay { summary, lines })
}

/// First index where `a` and `b` diverge (None when identical). Line
/// comparison is the byte-identity proof; the hash alone could theoretically
/// collide, so the in-process comparison is on the lines themselves.
fn first_divergence(a: &[String], b: &[String]) -> Option<usize> {
    let n = a.len().min(b.len());
    for i in 0..n {
        if a[i] != b[i] {
            return Some(i);
        }
    }
    if a.len() != b.len() {
        Some(n)
    } else {
        None
    }
}

/// The check: two fresh runs over `events` (byte-identity, MOD-10), then —
/// when a live summary exists — replay-vs-live identity (MOD-9).
pub fn check_day(
    date: &str,
    events: &[EventEnvelope],
    venues: &[Venue],
    symbols: &[SymbolId],
    cfg: &DeterminismConfig,
    fe_cfg: &FeaturesConfig,
    live: Option<&ReplaySummary>,
) -> Result<DeterminismVerdict, String> {
    let a = replay(date, events, venues, symbols, cfg, fe_cfg)?;
    let b = replay(date, events, venues, symbols, cfg, fe_cfg)?;
    let divergence = first_divergence(&a.lines, &b.lines);
    let self_consistent = divergence.is_none();
    if !self_consistent {
        return Ok(DeterminismVerdict {
            date: date.to_string(),
            strategy: cfg.strategy.clone(),
            seed: cfg.seed,
            event_count: events.len(),
            self_consistent: false,
            replayed_hash: a.summary.hash,
            replayed_lines: a.summary.lines,
            live_present: live.is_some(),
            live_matches: None,
            divergence_line: divergence,
            passed: false,
            reason: format!(
                "run 1 and run 2 diverged at decision {} — the decision path is not deterministic (PD-3 breach? wall clock / unseeded RNG / map-order iteration)",
                divergence.unwrap_or(0)
            ),
        });
    }

    let (live_present, live_matches, live_div) = match live {
        Some(l) => {
            // Byte-identity via the CONV-12 golden hash + the count
            // dimensions (a hash collision across differing lines is not a
            // real risk for gate purposes, and the in-process comparison above
            // is already on full lines).
            let same = l.hash == a.summary.hash
                && l.lines == a.summary.lines
                && l.intents == a.summary.intents
                && l.fills == a.summary.fills
                && l.event_count == a.summary.event_count;
            (true, Some(same), (!same).then_some(0))
        }
        None => (false, None, None),
    };
    let passed = live_matches.unwrap_or(true);
    let reason = match (live_present, live_matches) {
        (true, Some(true)) => format!(
            "two runs byte-identical; replay matches the live decision log (hash {})",
            a.summary.hash
        ),
        (true, Some(false)) => format!(
            "replay (hash {}) does not match the live decision log (hash {}) — online ≠ offline, G3 broken",
            a.summary.hash,
            live.map(|l| l.hash).unwrap_or(0)
        ),
        _ => format!(
            "two runs byte-identical; no live decision log for the day to compare (hash {})",
            a.summary.hash
        ),
    };
    Ok(DeterminismVerdict {
        date: date.to_string(),
        strategy: cfg.strategy.clone(),
        seed: cfg.seed,
        event_count: events.len(),
        self_consistent: true,
        replayed_hash: a.summary.hash,
        replayed_lines: a.summary.lines,
        live_present,
        live_matches,
        divergence_line: live_div,
        passed,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::{MarketEvent, OrderIntent, OrderKind, Side, SizeUnit, TimeInForce};
    use mp_strategies::strategy::RegimeMask;

    /// A strategy that emits one market order per feature update (seeded — so
    /// its decisions exercise the full intent→gate→fill path deterministically).
    struct EmittingStrat {
        next: u128,
    }
    impl Strategy for EmittingStrat {
        fn id(&self) -> StrategyId {
            StrategyId::new("emitter")
        }
        fn universe(&self) -> Universe {
            Universe::default()
        }
        fn subscriptions(&self) -> Vec<String> {
            vec!["*".to_string()]
        }
        fn warmup_ns(&self) -> i64 {
            0
        }
        fn declared_regime(&self) -> RegimeMask {
            RegimeMask::any()
        }
        fn on_feature(
            &mut self,
            u: &mp_features::FeatureUpdate,
            ctx: &mut dyn mp_strategies::Ctx,
        ) -> Vec<OrderIntent> {
            self.next += 1;
            let side = if ctx.next_u64() & 1 == 0 {
                Side::Buy
            } else {
                Side::Sell
            };
            vec![OrderIntent {
                intent_id: mp_core::IntentId(self.next),
                strategy: self.id(),
                venue: u.venue,
                symbol: u.symbol,
                side,
                kind: OrderKind::Market,
                qty: SizeUnit::Contracts(1.0),
                tif: TimeInForce::Ioc,
                reduce_only: false,
                tag: "emitter".into(),
            }]
        }
        fn with_params(
            &self,
            _: &std::collections::BTreeMap<String, f64>,
        ) -> Box<dyn Strategy> {
            Box::new(EmittingStrat { next: 0 })
        }
    }

    fn trade(venue: Venue, recv: i64, seq: u64, price: f64) -> EventEnvelope {
        EventEnvelope::new(
            venue,
            mp_core::SymbolId(0),
            recv,
            recv,
            seq,
            MarketEvent::Trade {
                price,
                qty: 1.0,
                side: Side::Buy,
                trade_id: seq,
            },
        )
    }

    fn events() -> Vec<EventEnvelope> {
        // Two symbols would need symbol remapping; the check runs on the
        // caller's already-shared ids, so one symbol suffices here.
        (1..=50)
            .map(|i| trade(Venue::Hyperliquid, i as i64 * 1_000_000_000, i, 100.0 + i as f64))
            .collect()
    }

    fn cfg() -> DeterminismConfig {
        DeterminismConfig {
            strategy: "carry-v1".into(),
            seed: DEFAULT_SEED,
            params: BTreeMap::new(),
        }
    }

    fn fe_cfg() -> FeaturesConfig {
        FeaturesConfig::default()
    }

    #[test]
    fn mod_10_decision_path_deterministic_two_runs_byte_identical() {
        let evs = events();
        let c = cfg();
        let a = replay("2026-08-13", &evs, &[Venue::Hyperliquid], &[mp_core::SymbolId(0)], &c, &fe_cfg())
            .unwrap();
        let b = replay("2026-08-13", &evs, &[Venue::Hyperliquid], &[mp_core::SymbolId(0)], &c, &fe_cfg())
            .unwrap();
        assert_eq!(a.lines, b.lines, "byte-identical decision logs");
        assert_eq!(a.summary.hash, b.summary.hash);
        assert_eq!(first_divergence(&a.lines, &b.lines), None);
    }

    #[test]
    fn mod_9_daily_determinism_replay_matches_live_summary() {
        let evs = events();
        let c = cfg();
        let v = check_day(
            "2026-08-13",
            &evs,
            &[Venue::Hyperliquid],
            &[mp_core::SymbolId(0)],
            &c,
            &fe_cfg(),
            None,
        )
        .unwrap();
        assert!(v.self_consistent, "{}", v.reason);
        assert!(v.passed, "{}", v.reason);
        assert!(!v.live_present);

        // With the day's own replay summary as the "live" log, identity holds.
        let live = ReplaySummary {
            date: "2026-08-13".into(),
            strategy: "carry-v1".into(),
            seed: DEFAULT_SEED,
            event_count: evs.len(),
            lines: v.replayed_lines,
            intents: 0,
            fills: 0,
            hash: v.replayed_hash,
        };
        let v2 = check_day(
            "2026-08-13",
            &evs,
            &[Venue::Hyperliquid],
            &[mp_core::SymbolId(0)],
            &c,
            &fe_cfg(),
            Some(&live),
        )
        .unwrap();
        assert!(v2.passed, "{}", v2.reason);
        assert_eq!(v2.live_matches, Some(true));
    }

    #[test]
    fn mod_9_determinism_diff_fails_when_live_hash_differs() {
        let evs = events();
        let c = cfg();
        let v = check_day(
            "2026-08-13",
            &evs,
            &[Venue::Hyperliquid],
            &[mp_core::SymbolId(0)],
            &c,
            &fe_cfg(),
            Some(&ReplaySummary {
                date: "2026-08-13".into(),
                strategy: "carry-v1".into(),
                seed: DEFAULT_SEED,
                event_count: evs.len(),
                lines: 1,
                intents: 1,
                fills: 0,
                hash: 0xDEAD, // a live log that disagrees with the replay
            }),
        )
        .unwrap();
        assert!(!v.passed, "live mismatch must fail");
        assert_eq!(v.live_matches, Some(false));
        assert!(v.reason.contains("does not match the live"), "{}", v.reason);
    }

    #[test]
    fn mod_11_determinism_config_rejects_unknown_keys() {
        assert!(DeterminismConfig::from_toml("strategy = \"carry-v1\"\n").is_ok());
        assert!(
            DeterminismConfig::from_toml("strategy = \"carry-v1\"\nstrat = \"x\"\n").is_err(),
            "unknown key must fail (CONV-16)"
        );
        assert!(
            DeterminismConfig::from_toml("strategy = \"nope\"\n").is_ok(),
            "strategy validity is checked at runtime"
        );
        let c = DeterminismConfig::from_toml("strategy = \"nope\"").unwrap();
        assert!(strategy_for(&c, &[], &[]).is_err(), "unknown strategy fails closed");
    }

    #[test]
    fn mod_10_emitting_strategy_is_deterministic_across_runs() {
        // The strongest form: a strategy that actually emits intents and
        // consumes the seeded RNG — full intent→verdict→fill path, twice,
        // must be byte-identical.
        let evs = events();
        let fe = engine_from_config(&fe_cfg()).unwrap();
        let strat = Box::new(EmittingStrat { next: 0 });
        let mut a = Backtester::from_strategies(fe, vec![strat], check_sim_config(), DEFAULT_SEED);
        a.stream(evs.iter().cloned());
        let fe = engine_from_config(&fe_cfg()).unwrap();
        let strat = Box::new(EmittingStrat { next: 0 });
        let mut b = Backtester::from_strategies(fe, vec![strat], check_sim_config(), DEFAULT_SEED);
        b.stream(evs.iter().cloned());
        assert_eq!(a.decision_log().lines(), b.decision_log().lines());
        assert_eq!(a.decision_log().hash(), b.decision_log().hash());
        assert!(a.decision_log().intent_count() > 0, "the emitter must actually trade");
    }
}
