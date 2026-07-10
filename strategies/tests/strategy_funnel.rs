//! Acceptance tests for spec 006. Test names embed requirement IDs (CONV-21).

use mp_core::{OrderIntent, StrategyId, SymbolId};
use mp_features::FeatureUpdate;
use mp_strategies::funnel::{FunnelError, Stage};
use mp_strategies::strategy::{Ctx, TimerId};
use mp_strategies::{CoinFlipStrategy, FunnelState, NullStrategy, Strategy};

/// Minimal deterministic Ctx for tests: seeded SplitMix64 rng, flat book.
struct TestCtx {
    now: i64,
    state: u64,
    timers: u64,
    logs: Vec<String>,
}
impl TestCtx {
    fn new(seed: u64) -> Self {
        Self {
            now: 0,
            state: seed,
            timers: 0,
            logs: Vec::new(),
        }
    }
}
impl Ctx for TestCtx {
    fn now_ns(&self) -> i64 {
        self.now
    }
    fn position(&self, _symbol: SymbolId) -> f64 {
        0.0
    }
    fn equity_allocated(&self) -> f64 {
        100_000.0
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn set_timer(&mut self, _after_ns: i64) -> TimerId {
        self.timers += 1;
        TimerId(self.timers)
    }
    fn log(&mut self, msg: &str) {
        self.logs.push(msg.to_string());
    }
}

fn feat(v: f64) -> FeatureUpdate {
    FeatureUpdate {
        feature: "cvd.bybit".into(),
        symbol: SymbolId(0),
        ts_ns: 1,
        value: v,
        ver: 1,
    }
}

fn run(s: &mut dyn Strategy, seed: u64, n: usize) -> Vec<OrderIntent> {
    let mut ctx = TestCtx::new(seed);
    let mut out = Vec::new();
    for i in 0..n {
        out.extend(s.on_feature(&feat(i as f64), &mut ctx));
    }
    out
}

#[test]
fn str_9_null_emits_nothing_coinflip_trades() {
    assert!(run(&mut NullStrategy, 1, 10).is_empty());
    assert!(!run(&mut CoinFlipStrategy::new(), 1, 10).is_empty());
}

#[test]
fn str_7_coinflip_is_deterministic_under_same_seed() {
    let a = run(&mut CoinFlipStrategy::new(), 42, 20);
    let b = run(&mut CoinFlipStrategy::new(), 42, 20);
    assert_eq!(a, b, "same seed ⇒ identical intents (CONV-11)");
    // Different seed should (almost surely) differ in side sequence.
    let c = run(&mut CoinFlipStrategy::new(), 7, 20);
    assert_ne!(a, c);
}

#[test]
fn str_2_hypothesis_required_to_leave_idea() {
    let mut s = FunnelState::register(StrategyId::new("x"), false);
    assert_eq!(
        s.promote(Stage::Hypothesis, false, vec![]),
        Err(FunnelError::MissingHypothesis)
    );
    // With a completed hypothesis it proceeds.
    let mut s2 = FunnelState::register(StrategyId::new("x"), true);
    assert!(s2.promote(Stage::Hypothesis, false, vec![]).is_ok());
}

#[test]
fn str_3_funnel_full_lifecycle_and_gates() {
    let mut s = FunnelState::register(StrategyId::new("carry-v1"), true);

    // Can't skip a stage.
    assert_eq!(
        s.promote(Stage::Backtest, false, vec![]),
        Err(FunnelError::NotNextStage)
    );

    s.promote(Stage::Hypothesis, false, vec![]).unwrap();
    s.promote(Stage::Backtest, false, vec![]).unwrap();

    // G1 Backtest→WalkForward needs evidence.
    assert_eq!(
        s.promote(Stage::WalkForward, false, vec![]),
        Err(FunnelError::MissingEvidence)
    );
    s.promote(Stage::WalkForward, false, vec!["run:wf1".into()])
        .unwrap();
    s.promote(Stage::Paper, false, vec!["run:oos1".into()])
        .unwrap();

    // G3 Paper→LiveSmall requires a human click — an agent (human=false) can't.
    assert_eq!(
        s.promote(Stage::LiveSmall, false, vec!["run:paper1".into()]),
        Err(FunnelError::NeedsHuman)
    );
    let t = s
        .promote(Stage::LiveSmall, true, vec!["run:paper1".into()])
        .unwrap();
    assert_eq!(t.actor, mp_strategies::Actor::Human);

    // Automatic demotion on a DD breach — no human needed.
    let d = s.demote(Stage::Paper, "dd budget breached").unwrap();
    assert_eq!(d.actor, mp_strategies::Actor::Auto);
    assert_eq!(s.stage, Stage::Paper);

    // Kill requires an autopsy; then terminal.
    assert_eq!(s.kill(false, "no edge"), Err(FunnelError::MissingAutopsy));
    s.kill(true, "no edge after costs").unwrap();
    assert_eq!(s.stage, Stage::Killed);
    assert_eq!(
        s.promote(Stage::Idea, true, vec![]),
        Err(FunnelError::Terminal)
    );
}

#[test]
fn str_5_transitions_journal_as_jsonl() {
    let mut s = FunnelState::register(StrategyId::new("x"), true);
    let t = s.promote(Stage::Hypothesis, false, vec![]).unwrap();
    let line = t.to_jsonl();
    assert!(line.contains("\"from\":\"Idea\""));
    assert!(line.contains("\"to\":\"Hypothesis\""));
    assert!(line.contains("\"actor\":\"Auto\""));
}
