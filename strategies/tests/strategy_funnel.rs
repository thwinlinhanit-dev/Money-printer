//! Acceptance tests for spec 006. Test names embed requirement IDs (CONV-21).

use mp_core::{OrderIntent, StrategyId, SymbolId};
use mp_features::FeatureUpdate;
use mp_strategies::funnel::{FunnelError, Stage};
use mp_strategies::strategy::{Ctx, TimerId};
use mp_strategies::{Autopsy, CoinFlipStrategy, EvidenceRef, FunnelState, NullStrategy, Strategy};

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
        feature: SymbolId(1),
        venue: mp_core::Venue::Bybit,
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

fn ev(run_id: &str) -> Vec<EvidenceRef> {
    vec![EvidenceRef {
        run_id: run_id.to_string(),
        created_ts_ns: NOW, // fresh
    }]
}
const NOW: i64 = 100 * 86_400_000_000_000; // day 100

#[test]
fn str_2_hypothesis_required_to_leave_idea() {
    let mut s = FunnelState::register(StrategyId::new("x"), false);
    assert_eq!(
        s.promote(Stage::Hypothesis, false, vec![], NOW),
        Err(FunnelError::MissingHypothesis)
    );
    // With a completed hypothesis it proceeds.
    let mut s2 = FunnelState::register(StrategyId::new("x"), true);
    assert!(s2.promote(Stage::Hypothesis, false, vec![], NOW).is_ok());
}

#[test]
fn str_3_funnel_full_lifecycle_and_gates() {
    let mut s = FunnelState::register(StrategyId::new("carry-v1"), true);

    // Can't skip a stage.
    assert_eq!(
        s.promote(Stage::Backtest, false, vec![], NOW),
        Err(FunnelError::NotNextStage)
    );

    s.promote(Stage::Hypothesis, false, vec![], NOW).unwrap();
    s.promote(Stage::Backtest, false, vec![], NOW).unwrap();

    // G1 Backtest→WalkForward needs evidence.
    assert_eq!(
        s.promote(Stage::WalkForward, false, vec![], NOW),
        Err(FunnelError::MissingEvidence)
    );
    s.promote(Stage::WalkForward, false, ev("run:wf1"), NOW)
        .unwrap();
    s.promote(Stage::Paper, false, ev("run:oos1"), NOW).unwrap();

    // G3 Paper→LiveSmall requires a human click — an agent (human=false) can't.
    assert_eq!(
        s.promote(Stage::LiveSmall, false, ev("run:paper1"), NOW),
        Err(FunnelError::NeedsHuman)
    );
    let t = s
        .promote(Stage::LiveSmall, true, ev("run:paper1"), NOW)
        .unwrap();
    assert_eq!(t.actor, mp_strategies::Actor::Human);

    // Automatic demotion on a DD breach — no human needed.
    let d = s.demote(Stage::Paper, "dd budget breached").unwrap();
    assert_eq!(d.actor, mp_strategies::Actor::Auto);
    assert_eq!(s.stage, Stage::Paper);

    // Kill requires an autopsy; then terminal.
    let empty = Autopsy {
        believed: String::new(),
        data_said: String::new(),
        lesson: String::new(),
    };
    assert_eq!(
        s.kill(&empty, "no edge"),
        Err(FunnelError::IncompleteAutopsy)
    );
    let autopsy = Autopsy {
        believed: "carry decays slower than fees".into(),
        data_said: "expectancy negative in 2x-cost column across all WF windows".into(),
        lesson: "fee sensitivity must be tested before paper".into(),
    };
    s.kill(&autopsy, "no edge after costs").unwrap();
    assert_eq!(s.stage, Stage::Killed);
    assert_eq!(
        s.promote(Stage::Idea, true, vec![], NOW),
        Err(FunnelError::Terminal)
    );
}

#[test]
fn str_5_transitions_journal_as_jsonl() {
    let mut s = FunnelState::register(StrategyId::new("x"), true);
    let t = s.promote(Stage::Hypothesis, false, vec![], NOW).unwrap();
    let line = t.to_jsonl();
    assert!(line.contains("\"from\":\"Idea\""));
    assert!(line.contains("\"to\":\"Hypothesis\""));
    assert!(line.contains("\"actor\":\"Auto\""));
}

#[test]
fn str_4_stale_evidence_is_refused() {
    let mut s = FunnelState::register(StrategyId::new("y"), true);
    s.promote(Stage::Hypothesis, false, vec![], NOW).unwrap();
    s.promote(Stage::Backtest, false, vec![], NOW).unwrap();
    // Evidence 31 days old at promotion time ⇒ refused (STR-4); the run must
    // be re-produced, not trusted forever.
    let stale = vec![EvidenceRef {
        run_id: "run:old".into(),
        created_ts_ns: NOW - 31 * 86_400_000_000_000,
    }];
    assert_eq!(
        s.promote(Stage::WalkForward, false, stale, NOW),
        Err(FunnelError::StaleEvidence)
    );
    // 29-day-old evidence is still valid.
    let fresh_enough = vec![EvidenceRef {
        run_id: "run:recent".into(),
        created_ts_ns: NOW - 29 * 86_400_000_000_000,
    }];
    assert!(s
        .promote(Stage::WalkForward, false, fresh_enough, NOW)
        .is_ok());
    // The run ids land in the state's evidence log (links to SIM-10 tracker).
    assert!(s.evidence.contains(&"run:recent".to_string()));
}

#[test]
fn str_6_autopsy_renders_the_kill_artifact() {
    let a = Autopsy {
        believed: "liq cascades overshoot".into(),
        data_said: "CAR flat after costs; CI includes zero".into(),
        lesson: "event studies before hypothesis, not after".into(),
    };
    let md = a.to_markdown(&StrategyId::new("liq-fade-v1"));
    for h in [
        "# AUTOPSY — liq-fade-v1",
        "## What we believed",
        "## What the data said",
        "## Lesson",
    ] {
        assert!(md.contains(h), "missing: {h}");
    }
    // An incomplete autopsy is not an autopsy.
    let hollow = Autopsy {
        believed: " ".into(),
        data_said: "x".into(),
        lesson: "y".into(),
    };
    assert!(!hollow.is_complete());
}

#[test]
fn str_1_strategy_trait_matches_design_and_ctx_exposes_no_io() {
    // STR-1 is chiefly compile-structural: strategies depend only on core +
    // features (PD-4, enforced by the guardrail dependency grep), and `Ctx`
    // offers exactly {now_ns, position, equity_allocated, next_u64, set_timer,
    // log} — no venue handles, no I/O, no wall clock. This test pins the
    // surface by exercising a Strategy solely through `&mut dyn Ctx`.
    use mp_strategies::{Ctx, NullStrategy, Strategy, TimerId};
    struct FixedCtx;
    impl Ctx for FixedCtx {
        fn now_ns(&self) -> i64 {
            42
        }
        fn position(&self, _s: mp_core::SymbolId) -> f64 {
            0.0
        }
        fn equity_allocated(&self) -> f64 {
            1_000.0
        }
        fn next_u64(&mut self) -> u64 {
            7
        }
        fn set_timer(&mut self, _after_ns: i64) -> TimerId {
            TimerId(1)
        }
        fn log(&mut self, _m: &str) {}
    }
    let mut s = NullStrategy;
    let u = mp_features::FeatureUpdate {
        feature: mp_core::SymbolId(1),
        venue: mp_core::Venue::Bybit,
        symbol: mp_core::SymbolId(0),
        ts_ns: 1,
        value: 1.0,
        ver: 1,
    };
    let intents = s.on_feature(&u, &mut FixedCtx);
    assert!(intents.is_empty());
    assert_eq!(s.id().0, "null");
}

#[test]
fn str_8_launch_strategies_have_written_hypotheses() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for dir in ["carry-v1", "trend-breadth-v1", "liq-fade-v1"] {
        let p = root.join(dir).join("hypothesis.md");
        let text = std::fs::read_to_string(&p)
            .unwrap_or_else(|_| panic!("{} must exist (STR-8)", p.display()));
        assert!(
            text.len() > 200,
            "{dir}/hypothesis.md must be a real hypothesis, not a stub"
        );
    }
}

#[test]
fn str_3_funnel_cli_gates_human_promotions_and_writes_autopsy() {
    let bin = env!("CARGO_BIN_EXE_funnel");
    let dir = std::env::temp_dir().join(format!("mpfunnel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let state = dir.join("funnel.json");
    let state_s = state.to_str().unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new(bin)
            .arg(state_s)
            .args(args)
            .output()
            .expect("run funnel cli")
    };

    assert!(run(&["register", "cli-test", "--hypothesis-complete"])
        .status
        .success());
    assert!(run(&["promote", "hypothesis"]).status.success());
    assert!(run(&["promote", "backtest"]).status.success());
    // Evidence gate refuses without evidence (exit 2).
    assert_eq!(run(&["promote", "walkforward"]).status.code(), Some(2));
    // Fresh evidence: use a far-future ts so staleness can't flake.
    let ev = format!("run:wf1:{}", i64::MAX - 1);
    assert!(run(&["promote", "walkforward", "--evidence", &ev])
        .status
        .success());
    assert!(run(&["promote", "paper", "--evidence", &ev])
        .status
        .success());
    // G3 without --i-am-human is refused (agents never click).
    assert_eq!(
        run(&["promote", "livesmall", "--evidence", &ev])
            .status
            .code(),
        Some(2)
    );
    assert!(
        run(&["promote", "livesmall", "--evidence", &ev, "--i-am-human"])
            .status
            .success()
    );
    // Kill demands the full autopsy and writes AUTOPSY.md next to the state.
    assert_eq!(run(&["kill", "--reason", "r"]).status.code(), Some(2));
    assert!(run(&[
        "kill",
        "--believed",
        "b",
        "--data-said",
        "d",
        "--lesson",
        "l",
        "--reason",
        "done"
    ])
    .status
    .success());
    assert!(dir.join("AUTOPSY.md").exists());
    // Every transition was journaled (STR-5).
    let journal = std::fs::read_to_string(format!("{state_s}.journal")).unwrap();
    assert!(journal.lines().count() >= 6);
    let _ = std::fs::remove_dir_all(&dir);
}
