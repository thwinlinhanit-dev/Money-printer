//! Acceptance tests for specs 037/038/039. Test names embed requirement IDs
//! (CONV-21). All fixtures are synthetic in-memory events — no network
//! (CONV-23).

use mp_core::event::{EventEnvelope, MarketEvent, OptionGreeks, OptionKind, Side};
use mp_core::{SymbolId, Venue};
use mp_features::options_flow::{
    bs_delta, flow_tenor, moneyness_bucket, FlowFeature, FlowMetric, FlowParams,
    MoneynessBucket,
};
use mp_features::options_greeks::{
    ChainScalar, ChainScalarFeature, GreeksAggregator, HigherOrderGreek,
};
use mp_features::options_iv::{vrp, IvAtm, IvIndex, IvPercentileFeature, IvSkew, IvTerm, VolRegime};
use mp_features::{FeatureEngine, FeaturesConfig, TickFeature};

const SEC: i64 = 1_000_000_000;
const DAY_NS: i64 = 86_400_000_000_000;
/// Fixed base time (~2025-06-15) — event-time only, never wall clock.
const T0: i64 = 1_750_000_000 * SEC;

fn ticker(
    recv: i64,
    u: &str,
    expiry: i64,
    strike: f64,
    kind: OptionKind,
    mark_iv: f64,
    open_interest: f64,
    spot: f64,
    greeks: (f64, f64, f64, f64), // (delta, gamma, theta, vega)
) -> EventEnvelope {
    let leg = mp_core::event::OptionLeg {
        underlying: u.to_string(),
        strike,
        expiry_ts_ns: expiry,
        kind,
    };
    EventEnvelope::new(
        Venue::Deribit,
        SymbolId(1),
        recv,
        recv,
        0,
        MarketEvent::OptionTicker {
            leg,
            mark_iv,
            mark_price: 1.0,
            underlying_price: spot,
            open_interest,
            greeks: Some(OptionGreeks {
                delta: greeks.0,
                gamma: greeks.1,
                theta: greeks.2,
                vega: greeks.3,
            }),
        },
    )
}

fn opt_trade(
    recv: i64,
    u: &str,
    expiry: i64,
    strike: f64,
    kind: OptionKind,
    price: f64,
    qty: f64,
    side: Side,
) -> EventEnvelope {
    let leg = mp_core::event::OptionLeg {
        underlying: u.to_string(),
        strike,
        expiry_ts_ns: expiry,
        kind,
    };
    EventEnvelope::new(
        Venue::Deribit,
        SymbolId(2),
        recv,
        recv,
        0,
        MarketEvent::OptionTrade { leg, price, qty, side, trade_id: 0 },
    )
}

fn value(engine: &FeatureEngine, ups: &[mp_features::FeatureUpdate], feat: &str) -> Option<f64> {
    let id = engine.name_to_id(feat)?;
    ups.iter().rev().find(|u| u.feature == id).map(|u| u.value)
}

#[test]
fn gre_1_catalog_registration() {
    let cfg: FeaturesConfig = toml::from_str(
        r#"
[options_greeks]
underlyings = ["BTC"]
"#,
    )
    .unwrap();
    let mut e = FeatureEngine::new(SEC);
    mp_features::register_options_families(&mut e, &cfg).unwrap();
    // One ticker batch lights the single-contract features up immediately.
    let ev = ticker(T0, "BTC", T0 + 30 * DAY_NS, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 2e-5, -100.0, 0.02));
    let ups = e.on_event(&ev);
    assert!(value(&e, &ups, "gex.net.btc").is_some());
    // Single strike chain: max pain is trivially that strike.
    assert!(value(&e, &ups, "gex.max_pain.btc").is_some());
    assert!(value(&e, &ups, "net.delta.btc").is_some());
    assert!(value(&e, &ups, "net.vega.btc").is_some());
    assert!(value(&e, &ups, "net.theta.btc").is_some());
}

#[test]
fn gre_2_gex_profile_signs_and_values() {
    let mut agg = GreeksAggregator::new(1.0);
    let exp = T0 + 30 * DAY_NS;
    let spot = 100_000.0;
    agg.on_ticker(&ticker(T0, "BTC", exp, 90_000.0, OptionKind::Call, 0.7, 10.0, spot, (0.8, 2e-5, -50.0, 0.01)), "BTC");
    agg.on_ticker(&ticker(T0, "BTC", exp, 100_000.0, OptionKind::Put, 0.4, 20.0, spot, (-0.45, 3e-5, -80.0, 0.01)), "BTC");
    agg.on_ticker(&ticker(T0, "BTC", exp, 100_000.0, OptionKind::Call, 0.6, 5.0, spot, (0.5, 5e-5, -70.0, 0.02)), "BTC");
    let profile = agg.gex_profile("BTC");
    assert_eq!(profile.len(), 2);
    let gex_90 = 2e-5 * 10.0 * spot * spot; // +2e6 (call positive)
    let gex_100_put = -3e-5 * 20.0 * spot * spot; // −6e6 (put negative)
    let gex_100_call = 5e-5 * 5.0 * spot * spot; // +2.5e6
    assert!((profile[0].0 - 90_000.0).abs() < 1e-9);
    assert!((profile[0].1 - gex_90).abs() <= 1e-6);
    assert!((profile[1].0 - 100_000.0).abs() < 1e-9);
    assert!((profile[1].1 - (gex_100_put + gex_100_call)).abs() <= 1e-6);
}

#[test]
fn gre_2_gex_replaces_on_update() {
    let mut agg = GreeksAggregator::new(1.0);
    let exp = T0 + 30 * DAY_NS;
    let spot = 100_000.0;
    agg.on_ticker(&ticker(T0, "BTC", exp, 100_000.0, OptionKind::Call, 0.6, 10.0, spot, (0.5, 1e-5, 0.0, 0.0)), "BTC");
    assert!((agg.net_gex("BTC").unwrap() - 1e6).abs() < 1e-3);
    // Same contract re-ticked with OI=25: REPLACES (2.5e6), not appends (3.5e6).
    agg.on_ticker(&ticker(T0 + SEC, "BTC", exp, 100_000.0, OptionKind::Call, 0.6, 25.0, spot, (0.5, 1e-5, 0.0, 0.0)), "BTC");
    assert!((agg.net_gex("BTC").unwrap() - 2.5e6).abs() < 1e-3);
    assert_eq!(agg.gex_profile("BTC").len(), 1);
}

#[test]
fn gre_3_max_pain_correct_on_fixture() {
    let mut agg = GreeksAggregator::new(1.0);
    let exp = T0 + 30 * DAY_NS;
    let s = |strike: f64, kind, oi: f64| {
        ticker(T0, "BTC", exp, strike, kind, 0.5, oi, 100_000.0, (0.5, 0.0, 0.0, 0.0))
    };
    // Calls: OI@100k=10, OI@110k=5; Puts: OI@90k=8, OI@100k=12.
    agg.on_ticker(&s(100_000.0, OptionKind::Call, 10.0), "BTC");
    agg.on_ticker(&s(110_000.0, OptionKind::Call, 5.0), "BTC");
    agg.on_ticker(&s(90_000.0, OptionKind::Put, 8.0), "BTC");
    agg.on_ticker(&s(100_000.0, OptionKind::Put, 12.0), "BTC");
    // Payout(S): 90k→120, 100k→0, 110k→100 ⇒ max pain = 100k.
    assert!((agg.max_pain("BTC").unwrap() - 100_000.0).abs() < 1e-9);
}

#[test]
fn gre_4_implied_prob_sums_to_one_and_symmetric() {
    let mut agg = GreeksAggregator::new(1.0);
    let exp = T0 + 30 * DAY_NS;
    let spot = 100_000.0;
    // Symmetric call-delta surface: deltas sum (as probabilities) around ATM.
    for (k, d) in [(80e3, 0.9), (90e3, 0.75), (100e3, 0.5), (110e3, 0.25), (120e3, 0.1)] {
        agg.on_ticker(&ticker(T0, "BTC", exp, k, OptionKind::Call, 0.6, 10.0, spot, (d, 1e-6, 0.0, 0.0)), "BTC");
    }
    let dist = agg.implied_prob_distribution("BTC").unwrap();
    let total: f64 = dist.iter().map(|(_, m)| m).sum();
    assert!((total - 1.0).abs() <= 1e-6);
    // Symmetric chain → symmetric density around the middle bucket.
    let n = dist.len();
    for i in 0..n / 2 {
        assert!((dist[i].1 - dist[n - 1 - i].1).abs() < 1e-9);
    }
}

#[test]
fn gre_5_higher_greeks_suppressed_until_warmup_then_correct() {
    let mut agg = GreeksAggregator::new(1.0);
    let exp = T0 + 30 * DAY_NS;
    let spot = 100_000.0;
    // Snapshot 1: net delta = 0.5×10 = 5; mean IV = 0.60.
    agg.on_ticker(&ticker(T0, "BTC", exp, 100_000.0, OptionKind::Call, 0.60, 10.0, spot, (0.5, 0.0, 0.0, 0.0)), "BTC");
    assert!(agg.advance_fd("BTC", T0).is_none()); // warmup: no emission
    // Snapshot 2: net delta = 0.6×10 = 6; mean IV = 0.62 → Δ=+1 / Δσ=+0.02.
    agg.on_ticker(&ticker(T0 + SEC, "BTC", exp, 100_000.0, OptionKind::Call, 0.62, 10.0, spot, (0.6, 0.0, 0.0, 0.0)), "BTC");
    let (vanna, _volga, charm) = agg.advance_fd("BTC", T0 + SEC).unwrap();
    assert!((vanna.unwrap() - 50.0).abs() < 1e-9); // 1 / 0.02
    assert!(charm.unwrap() > 0.0); // +1 delta per snapshot interval
}

#[test]
fn gre_8_check_config_rejects_unknown_fields() {
    let res: Result<FeaturesConfig, _> = toml::from_str(
        r#"
[options_greeks]
underlyings = ["BTC"]
bad_key = 1
"#,
    );
    assert!(res.is_err());
}

#[test]
fn gre_10_underlying_price_from_ticker() {
    // GEX uses the ticker's own underlying_price (GRE-10): two identical
    // contracts differing ONLY in the ticker's spot produce different GEX —
    // and the ratio is exactly spot².
    let mut a = GreeksAggregator::new(1.0);
    let mut b = GreeksAggregator::new(1.0);
    let exp = T0 + 30 * DAY_NS;
    a.on_ticker(&ticker(T0, "BTC", exp, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 2e-5, 0.0, 0.0)), "BTC");
    b.on_ticker(&ticker(T0, "BTC", exp, 100_000.0, OptionKind::Call, 0.6, 10.0, 120_000.0, (0.5, 2e-5, 0.0, 0.0)), "BTC");
    let ratio = b.net_gex("BTC").unwrap() / a.net_gex("BTC").unwrap();
    assert!((ratio - (1.2_f64).powi(2)).abs() < 1e-9);
}

#[test]
fn gre_11_per_expiry_decomposition_sums_to_aggregate() {
    let mut agg = GreeksAggregator::new(1.0);
    for exp_days in [7_i64, 30, 90] {
        let exp = T0 + exp_days * DAY_NS;
        agg.on_ticker(&ticker(T0, "BTC", exp, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, -10.0, 0.01)), "BTC");
        agg.on_ticker(&ticker(T0, "BTC", exp, 110_000.0, OptionKind::Put, 0.4, 5.0, 100_000.0, (-0.3, 1e-5, -20.0, 0.01)), "BTC");
    }
    let by_expiry = agg.net_greeks_by_expiry("BTC");
    assert_eq!(by_expiry.len(), 3);
    let sum: [f64; 3] = by_expiry.iter().fold([0.0; 3], |acc, (_, v)| {
        [acc[0] + v[0], acc[1] + v[1], acc[2] + v[2]]
    });
    assert!((sum[0] - agg.net_delta("BTC").unwrap()).abs() < 1e-9);
    assert!((sum[1] - agg.net_vega("BTC").unwrap()).abs() < 1e-9);
    assert!((sum[2] - agg.net_theta("BTC").unwrap()).abs() < 1e-9);
}

#[test]
fn gre_12_cross_underlying_isolation() {
    let mut f = ChainScalarFeature::new(ChainScalar::GexNet, "BTC", 1.0);
    let exp = T0 + 30 * DAY_NS;
    // ETH ticker must not move the BTC feature.
    assert!(f.on_event(&ticker(T0, "ETH", exp, 5_000.0, OptionKind::Call, 0.6, 10.0, 5_000.0, (0.5, 1e-4, 0.0, 0.0))).is_none());
    let btc_v = 2e-5 * 20.0 * 100_000.0 * 100_000.0; // γ×OI×spot²
    assert!((f.on_event(&ticker(T0 + SEC, "BTC", exp, 100_000.0, OptionKind::Call, 0.6, 20.0, 100_000.0, (0.5, 2e-5, 0.0, 0.0))).unwrap() - btc_v).abs() < 1e-3);
    // And an ETH update still leaves BTC's stored chain untouched.
    f.on_event(&ticker(T0 + 2 * SEC, "ETH", exp, 5_000.0, OptionKind::Put, 0.6, 99.0, 5_000.0, (-0.5, 1e-3, 0.0, 0.0)));
    assert!((f.on_event(&ticker(T0 + 3 * SEC, "BTC", exp, 100_000.0, OptionKind::Call, 0.6, 20.0, 100_000.0, (0.5, 2e-5, 0.0, 0.0))).unwrap() - btc_v).abs() < 1e-3);
}

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn gre_9_proptest_gex_bounded_by_oi_and_spot(
        gamma in 0.0..1e-3,
        oi in 0.0..10_000.0,
        spot in 1.0..1_000_000.0,
        is_put in proptest::bool::ANY,
    ) {
        use mp_features::options_greeks::{gex_at, TickerSnap};
        let snap = TickerSnap {
            mark_iv: 0.6,
            mark_price: 1.0,
            spot,
            open_interest: oi,
            greeks: Some(OptionGreeks { delta: 0.5, gamma, theta: 0.0, vega: 0.01 }),
        };
        if let Some(g) = gex_at(&snap, is_put, 1.0) {
            let bound = gamma * oi * spot * spot * 1.01;
            prop_assert!(g.abs() <= bound);
            if g != 0.0 {
                prop_assert_eq!(g > 0.0, !is_put);
            }
        } else {
            prop_assert!(gamma == 0.0 || oi == 0.0);
        }
    }
}

// ===========================================================================
// Spec 038 — IV surface (IVS)
// ===========================================================================

#[test]
fn ivs_2_atm_iv_interpolation_correct_and_uses_ticker_spot() {
    let mut f = IvAtm::new("BTC");
    let exp = T0 + 30 * DAY_NS;
    // Strikes 90k (IV .80) / 100k (IV .60), ticker spot 95k → interpolated .70.
    let e1 = f.on_event(&ticker(T0, "BTC", exp, 90_000.0, OptionKind::Call, 0.80, 10.0, 95_000.0, (0.6, 1e-5, 0.0, 0.0)));
    let e2 = f.on_event(&ticker(T0, "BTC", exp, 100_000.0, OptionKind::Call, 0.60, 10.0, 95_000.0, (0.4, 1e-5, 0.0, 0.0)));
    let v = f.on_event(&ticker(T0 + SEC, "BTC", exp, 100_000.0, OptionKind::Put, 0.60, 5.0, 95_000.0, (-0.4, 1e-5, 0.0, 0.0)));
    assert!((v.unwrap() - 0.70).abs() <= 1e-10, "ivs_2 got {v:?} spot={:?} (e1={e1:?} e2={e2:?})", f.aggregator().chains().spot("btc"));
    // IVS-2/GRE-10: the spot comes from the ticker — re-tick with spot 92.5k
    // and the interpolation point moves (weights 2.5/7.5 → .75).
    let v2 = f.on_event(&ticker(T0 + 2 * SEC, "BTC", exp, 100_000.0, OptionKind::Put, 0.60, 5.0, 92_500.0, (-0.4, 1e-5, 0.0, 0.0)));
    assert!((v2.unwrap() - 0.75).abs() <= 1e-10, "ivs_2 v2 got {v2:?}");
}

#[test]
fn ivs_3_term_structure_falls_back_to_nearest() {
    use mp_features::options_iv::IvTerm;
    let mut f = IvTerm::new("BTC", "1m", 30.0);
    let exp28 = T0 + 28 * DAY_NS;
    let exp40 = T0 + 40 * DAY_NS;
    f.on_event(&ticker(T0, "BTC", exp28, 100_000.0, OptionKind::Call, 0.55, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
    f.on_event(&ticker(T0, "BTC", exp40, 100_000.0, OptionKind::Call, 0.75, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
    // No exact 30d expiry → nearest is 28d → ATM IV = 0.55 (not interpolated
    // across expiries, not synthesized — IVS-3).
    let v = f.on_event(&ticker(T0 + SEC, "BTC", exp28, 100_000.0, OptionKind::Put, 0.55, 5.0, 100_000.0, (-0.5, 1e-5, 0.0, 0.0)));
    assert!((v.unwrap() - 0.55).abs() < 1e-10);
}

#[test]
fn ivs_4_risk_reversal_sign_and_value() {
    // Symmetric smile: call IV @ |Δ|=.25 == put IV @ |Δ|=.25 → RR = 0.
    let mut f = IvSkew::risk_reversal("BTC");
    let exp = T0 + 30 * DAY_NS;
    let s = 100_000.0;
    f.on_event(&ticker(T0, "BTC", exp, 90_000.0, OptionKind::Call, 0.70, 10.0, s, (0.25, 1e-5, 0.0, 0.0)));
    f.on_event(&ticker(T0, "BTC", exp, 100_000.0, OptionKind::Call, 0.60, 10.0, s, (0.5, 1e-5, 0.0, 0.0)));
    f.on_event(&ticker(T0, "BTC", exp, 110_000.0, OptionKind::Put, 0.70, 10.0, s, (-0.25, 1e-5, 0.0, 0.0)));
    f.on_event(&ticker(T0, "BTC", exp, 100_000.0, OptionKind::Put, 0.60, 10.0, s, (-0.5, 1e-5, 0.0, 0.0)));
    assert!((f.on_event(&ticker(T0 + SEC, "BTC", exp, 100_000.0, OptionKind::Call, 0.60, 5.0, s, (0.5, 1e-5, 0.0, 0.0))).unwrap() - 0.0).abs() < 1e-10);
    // Put wing richer: put 25Δ IV = .80 vs call 25Δ = .70 → RR = −0.10.
    let mut g = IvSkew::risk_reversal("ETH");
    let exp2 = T0 + 30 * DAY_NS;
    g.on_event(&ticker(T0, "ETH", exp2, 2_000.0, OptionKind::Call, 0.70, 10.0, 3_000.0, (0.25, 1e-5, 0.0, 0.0)));
    g.on_event(&ticker(T0, "ETH", exp2, 3_000.0, OptionKind::Put, 0.80, 10.0, 3_000.0, (-0.25, 1e-5, 0.0, 0.0)));
    let rr = g.on_event(&ticker(T0 + SEC, "ETH", exp2, 3_000.0, OptionKind::Call, 0.60, 5.0, 3_000.0, (0.5, 1e-5, 0.0, 0.0))).unwrap();
    assert!((rr - (-0.10)).abs() < 1e-10);
}

#[test]
fn ivs_5_vrp_positive_when_iv_above_rv_and_suppressed_without_rv() {
    assert!((vrp(0.8, 0.6).unwrap() - 0.2).abs() < 1e-12);
    assert!(vrp(f64::NAN, 0.6).is_none()); // fail-closed (IVS-5)
    assert!(vrp(0.8, f64::NAN).is_none());
}

#[test]
fn ivs_7_dvol_positive_finite_and_zero_oi_excluded() {
    let mut f = IvIndex::new("BTC");
    let exp = T0 + 30 * DAY_NS;
    let s = 100_000.0;
    // OI=0 strike must not contribute; OI-weighted variance mean of
    // (.50 w=30) and (.70 w=10): sqrt((30·.25 + 10·.49)/40) ≈ 0.559017.
    f.on_event(&ticker(T0, "BTC", exp, 90_000.0, OptionKind::Call, 0.50, 0.0, s, (0.6, 1e-5, 0.0, 0.0)));
    f.on_event(&ticker(T0, "BTC", exp, 95_000.0, OptionKind::Call, 0.50, 30.0, s, (0.6, 1e-5, 0.0, 0.0)));
    let v = f.on_event(&ticker(T0 + SEC, "BTC", exp, 105_000.0, OptionKind::Call, 0.70, 10.0, s, (0.6, 1e-5, 0.0, 0.0)));
    assert!((v.unwrap() - ((30.0 * 0.25 + 10.0 * 0.49) / 40.0_f64).sqrt()).abs() < 1e-12);
}

#[test]
fn ivs_6_regime_thresholds_and_ivs_11_percentile_skips_missing_days() {
    // 79 observed days all at IV = 0.50 (days 1..=79), then a gap (day 80
    // missing), then day 81 samples.
    let mut pct = IvPercentileFeature::percentile("BTC", 90);
    let mut reg = IvPercentileFeature::regime("BTC", 90);
    // Long-dated expiry: the test advances 83 days, so the contract must
    // still be live (nearest-expiry selection ignores expired tenors).
    let exp = T0 + 365 * DAY_NS;
    let s = |ts: i64, iv: f64| {
        ticker(ts, "BTC", exp, 100_000.0, OptionKind::Call, iv, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0))
    };
    for d in 1..=79 {
        pct.on_event(&s(T0 + d * DAY_NS, 0.50));
        reg.on_event(&s(T0 + d * DAY_NS, 0.50));
    }
    // Day 81 (day 80 skipped): current IV above ALL 79 past days →
    // percentile 1.0 with denominator n=79, NOT a diluted n=80/90 (IVS-11).
    let hi = ticker(T0 + 81 * DAY_NS, "BTC", exp, 100_000.0, OptionKind::Call, 0.55, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0));
    assert!((pct.on_event(&hi).unwrap() - 1.0).abs() < 1e-12);
    // ≥66th percentile → Rich → REVERSE-ORDINAL encoding 0.
    assert_eq!(reg.on_event(&hi).unwrap(), VolRegime::Rich.encode());
    // Day 82 sample equal to most of history: below = 0, equal = 79, plus
    // day-81's 0.55 is ABOVE current → pct = 0.5×79/80 = 0.49375 → Fair.
    let eq = ticker(T0 + 82 * DAY_NS, "BTC", exp, 100_000.0, OptionKind::Call, 0.50, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0));
    assert!((pct.on_event(&eq).unwrap() - 0.5 * 79.0 / 80.0).abs() < 1e-12);
    assert_eq!(reg.on_event(&eq).unwrap(), VolRegime::Fair.encode());
    // Below all history → 0.0 ≤ 33rd → Cheap (2).
    let lo = ticker(T0 + 83 * DAY_NS, "BTC", exp, 100_000.0, OptionKind::Call, 0.45, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0));
    assert!((pct.on_event(&lo).unwrap() - 0.0).abs() < 1e-12);
    assert_eq!(reg.on_event(&lo).unwrap(), VolRegime::Cheap.encode());
}

#[test]
fn ivs_8_deterministic_golden() {
    let replay = || {
        let mut f = IvAtm::new("BTC");
        let exp = T0 + 30 * DAY_NS;
        let mut ups = Vec::new();
        for i in 0..5 {
            let iv = 0.60 + 0.01 * i as f64;
            f.on_event(&ticker(T0 + i * SEC, "BTC", exp, 100_000.0, OptionKind::Call, iv, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
            ups.push(f.on_event(&ticker(T0 + i * SEC + 1, "BTC", exp, 100_000.0, OptionKind::Put, iv, 10.0, 100_000.0, (-0.5, 1e-5, 0.0, 0.0))).unwrap());
        }
        ups
    };
    let a = replay();
    let b = replay();
    // Bit-identical across replays (CONV-9/ivs_8).
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
}

#[test]
fn ivs_12_cross_underlying_isolation() {
    let mut f = IvAtm::new("BTC");
    let exp = T0 + 30 * DAY_NS;
    // ETH-only chain: BTC feature must stay suppressed.
    f.on_event(&ticker(T0, "ETH", exp, 3_000.0, OptionKind::Call, 0.70, 10.0, 3_000.0, (0.5, 1e-5, 0.0, 0.0)));
    assert!(f.on_event(&ticker(T0 + SEC, "ETH", exp, 3_000.0, OptionKind::Put, 0.70, 5.0, 3_000.0, (-0.5, 1e-5, 0.0, 0.0))).is_none());
}

// ===========================================================================
// Spec 039 — options flow aggregator (OFI)
// ===========================================================================

fn flow_params() -> FlowParams {
    FlowParams::default()
}

fn flow_feature(metric: FlowMetric, window_ns: i64) -> FlowFeature {
    FlowFeature::new(metric, "BTC", window_ns, flow_params())
}

const EXP: i64 = T0 + 30 * DAY_NS; // 30d expiry → monthly tenor

#[test]
fn ofi_2_block_detection_correct() {
    let mut f = flow_feature(FlowMetric::Block, 60 * SEC);
    // $150k buy call → block (+150k); $50k → not a block.
    let spot = ticker(T0, "BTC", EXP, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0));
    f.on_event(&spot);
    assert!(f.on_event(&opt_trade(T0 + SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_500.0, 100.0, Side::Buy)).is_none()); // in-window
    let closed = f.on_event(&opt_trade(T0 + 61 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 500.0, 100.0, Side::Buy)).unwrap();
    assert!((closed - 150_000.0).abs() < 1e-6); // only the $150k block counted
}

#[test]
fn ofi_3_net_premium_signs_correct() {
    let mut f = flow_feature(FlowMetric::NetPremium, 60 * SEC);
    let spot = ticker(T0, "BTC", EXP, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0));
    let spot_put = ticker(T0, "BTC", EXP, 95_000.0, OptionKind::Put, 0.5, 10.0, 100_000.0, (-0.3, 1e-5, 0.0, 0.0));
    f.on_event(&spot);
    f.on_event(&spot_put);
    // Window 1: buy call @ $1000 × 50 = +$50k (buy aggressor = +premium).
    f.on_event(&opt_trade(T0 + SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 50.0, Side::Buy));
    // This trade lands in window 2 → CLOSES window 1 → emit +50_000.
    let npf = f.on_event(&opt_trade(T0 + 61 * SEC, "BTC", EXP, 95_000.0, OptionKind::Put, 800.0, 25.0, Side::Sell)).unwrap();
    assert!((npf - 50_000.0).abs() < 1e-6);
    // Sell put @ $800 × 25 = −$20k; that was window 2's only trade.
    let npf2 = f.on_event(&opt_trade(T0 + 121 * SEC, "BTC", EXP, 95_000.0, OptionKind::Put, 800.0, 25.0, Side::Sell)).unwrap();
    assert!((npf2 - (-20_000.0)).abs() < 1e-6);
}

#[test]
fn ofi_4_delta_adjusted_flow_signs_and_suppression() {
    // Buy call (δ +0.5) → +; buy put (δ −0.4) → −; sell put (δ −0.4) → +;
    // |δ| > 1 trade suppressed (CONV-8).
    let mut f = flow_feature(FlowMetric::NetDelta, 60 * SEC);
    f.on_event(&ticker(T0, "BTC", EXP, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
    f.on_event(&ticker(T0, "BTC", EXP, 100_000.0, OptionKind::Put, 0.5, 10.0, 100_000.0, (-0.4, 1e-5, 0.0, 0.0)));
    f.on_event(&ticker(T0, "BTC", EXP, 90_000.0, OptionKind::Put, 0.5, 10.0, 100_000.0, (-1.5, 1e-5, 0.0, 0.0))); // |δ|>1
    // w1: buy call $100k × 0.5 = +50k.
    f.on_event(&opt_trade(T0 + SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy));
    let d1 = f.on_event(&opt_trade(T0 + 61 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy)).unwrap();
    assert!((d1 - 50_000.0).abs() < 1e-6);
    // w2: buy put −40k; the $100k |δ|>1 trade is suppressed (contributes 0);
    // PLUS the w1-closing trade itself (+50k) landed in w2 → total +10k.
    f.on_event(&opt_trade(T0 + 61 * SEC, "BTC", EXP, 100_000.0, OptionKind::Put, 1_000.0, 100.0, Side::Buy));
    f.on_event(&opt_trade(T0 + 62 * SEC, "BTC", EXP, 90_000.0, OptionKind::Put, 1_000.0, 100.0, Side::Buy));
    let d2 = f.on_event(&opt_trade(T0 + 121 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy)).unwrap();
    assert!((d2 - 10_000.0).abs() < 1e-6);
    // w3: sell put (+40k) plus the w2-closing trade (+50k) = +90k.
    f.on_event(&opt_trade(T0 + 121 * SEC, "BTC", EXP, 100_000.0, OptionKind::Put, 1_000.0, 100.0, Side::Sell));
    let d3 = f.on_event(&opt_trade(T0 + 181 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy)).unwrap();
    assert!((d3 - 90_000.0).abs() < 1e-6);
}

#[test]
fn ofi_4_bs_delta_fallback_used_without_ticker_match() {
    // No ticker for the 105k contract → BS delta from (spot=100k, iv=0.6,
    // T≈30d) — strictly between 0 and notional.
    let mut f = flow_feature(FlowMetric::NetDelta, 60 * SEC);
    // Window-ALIGNED base: T0 is not a multiple of 60s, and misaligned
    // buckets would silently pull the w1 trade into w2.
    let wbase = (T0 / (60 * SEC)) * (60 * SEC);
    f.on_event(&ticker(wbase, "BTC", EXP, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
    // w1: two ticker-matched trades (K=100k, δ=0.5) → +100k total.
    f.on_event(&opt_trade(wbase + SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy));
    f.on_event(&opt_trade(wbase + 30 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy));
    // The BS-fallback trade (K=105k, NO ticker) opens w2 as its ONLY member.
    f.on_event(&opt_trade(wbase + 61 * SEC, "BTC", EXP, 105_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy));
    // Register the |δ|>1 contract, then close w2 with a trade on it — it
    // contributes exactly 0 (suppressed), so `d` is purely the BS fallback.
    f.on_event(&ticker(wbase + 70 * SEC, "BTC", EXP, 90_000.0, OptionKind::Put, 0.5, 10.0, 100_000.0, (-1.5, 1e-5, 0.0, 0.0)));
    let d = f.on_event(&opt_trade(wbase + 121 * SEC, "BTC", EXP, 90_000.0, OptionKind::Put, 1_000.0, 100.0, Side::Buy)).unwrap();
    // Exact event-time T as the implementation computes it.
    let t_years = (EXP - (wbase + 61 * SEC)) as f64 / (365.25 * 86_400_000_000_000.0);
    let expected = 100_000.0 * bs_delta(100_000.0, 105_000.0, t_years, 0.6, OptionKind::Call).unwrap();
    assert!((d - expected).abs() < 1e-6, "ofi_4_bs d={d} expected={expected} t={t_years}");
    assert!(expected > 0.0 && expected < 100_000.0);
}

#[test]
fn ofi_5_moneyness_classification_and_kind_aware_regression() {
    let atm = 0.02;
    let itm_cap = 0.10;
    let otm_cap = 0.50;
    let s = 100_000.0;
    // Spec examples: 105k call → OTM; 102k put → ITM. (Review note: the
    // spec's original "98k put → ITM" was factually wrong — a 98k put at
    // spot 100k is OTM, max(0, K−S) = 0.)
    assert_eq!(moneyness_bucket(OptionKind::Call, 105_000.0, s, atm, itm_cap, otm_cap), Some(MoneynessBucket::Otm));
    assert_eq!(moneyness_bucket(OptionKind::Put, 102_000.0, s, atm, itm_cap, otm_cap), Some(MoneynessBucket::Itm));
    // And indeed: 98k put with spot at 100k is OTM.
    assert_eq!(moneyness_bucket(OptionKind::Put, 98_000.0, s, atm, itm_cap, otm_cap), Some(MoneynessBucket::Otm));
    // REGRESSION (review fix): 104k call with spot 100k is OTM — the naive
    // |K/S − 1| < band formula would call it ITM.
    assert_eq!(moneyness_bucket(OptionKind::Call, 104_000.0, s, atm, itm_cap, otm_cap), Some(MoneynessBucket::Otm));
    // And the mirror: 104k put IS ITM.
    assert_eq!(moneyness_bucket(OptionKind::Put, 104_000.0, s, atm, itm_cap, otm_cap), Some(MoneynessBucket::Itm));
    // ATM band: ±2% regardless of kind/side.
    assert_eq!(moneyness_bucket(OptionKind::Call, 101_000.0, s, atm, itm_cap, otm_cap), Some(MoneynessBucket::Atm));
    assert_eq!(moneyness_bucket(OptionKind::Put, 99_000.0, s, atm, itm_cap, otm_cap), Some(MoneynessBucket::Atm));
    // Deep OTM excluded: 150k call (50% OTM) and beyond.
    assert_eq!(moneyness_bucket(OptionKind::Call, 149_000.0, s, atm, itm_cap, otm_cap), Some(MoneynessBucket::Otm));
    assert_eq!(moneyness_bucket(OptionKind::Call, 151_000.0, s, atm, itm_cap, otm_cap), None);
}

#[test]
fn ofi_5_deep_otm_excluded_from_aggregation() {
    let mut f = flow_feature(FlowMetric::OtmFlow, 60 * SEC);
    f.on_event(&ticker(T0, "BTC", EXP, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
    // Normal OTM trade counts; deep-OTM (200% away) does not.
    f.on_event(&opt_trade(T0 + SEC, "BTC", EXP, 130_000.0, OptionKind::Call, 100.0, 100.0, Side::Buy)); // +$10k
    f.on_event(&opt_trade(T0 + SEC, "BTC", EXP, 300_000.0, OptionKind::Call, 5.0, 20_000.0, Side::Buy)); // excluded
    let otm = f.on_event(&opt_trade(T0 + 61 * SEC, "BTC", EXP, 130_000.0, OptionKind::Call, 100.0, 100.0, Side::Buy)).unwrap();
    assert!((otm - 10_000.0).abs() < 1e-6);
}

#[test]
fn ofi_6_tenor_classification() {
    // Pure classifier: 3d→weekly, 20d→monthly, 90d→quarterly, 270d→leap.
    assert_eq!(flow_tenor(3.0, 7.0, 45.0, 180.0), "weekly");
    assert_eq!(flow_tenor(20.0, 7.0, 45.0, 180.0), "monthly");
    assert_eq!(flow_tenor(90.0, 7.0, 45.0, 180.0), "quarterly");
    assert_eq!(flow_tenor(270.0, 7.0, 45.0, 180.0), "leap");
    // Feature-level: trades on different expiries land in the right bucket.
    let mut weekly = flow_feature(FlowMetric::WeeklyFlow, 60 * SEC);
    let mut monthly = flow_feature(FlowMetric::MonthlyFlow, 60 * SEC);
    let exp3 = T0 + 3 * DAY_NS;
    for f in [&mut weekly, &mut monthly] {
        f.on_event(&ticker(T0, "BTC", exp3, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
    }
    weekly.on_event(&opt_trade(T0 + SEC, "BTC", exp3, 100_000.0, OptionKind::Call, 1_000.0, 50.0, Side::Buy));
    monthly.on_event(&opt_trade(T0 + SEC, "BTC", exp3, 100_000.0, OptionKind::Call, 1_000.0, 50.0, Side::Buy));
    let w = weekly.on_event(&opt_trade(T0 + 61 * SEC, "BTC", exp3, 100_000.0, OptionKind::Call, 1_000.0, 50.0, Side::Buy)).unwrap();
    let m = monthly.on_event(&opt_trade(T0 + 61 * SEC, "BTC", exp3, 100_000.0, OptionKind::Call, 1_000.0, 50.0, Side::Buy)).unwrap();
    assert!((w - 50_000.0).abs() < 1e-6); // 3d expiry → weekly
    assert!((m - 0.0).abs() < 1e-12); // nothing monthly in that window
}

#[test]
fn ofi_7_acceleration_suppressed_until_warmup() {
    let mut f = flow_feature(FlowMetric::Acceleration, 60 * SEC);
    f.on_event(&ticker(T0, "BTC", EXP, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
    // w1: +$50k; closes on the w2 trade → first close has NO predecessor.
    f.on_event(&opt_trade(T0 + SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 50.0, Side::Buy));
    assert!(f.on_event(&opt_trade(T0 + 61 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 50.0, Side::Buy)).is_none());
    // w3 trade closes w2 (+50k) → acceleration = 50k − 50k = 0.
    let a1 = f.on_event(&opt_trade(T0 + 121 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy)).unwrap();
    assert!((a1 - 0.0).abs() < 1e-9);
    // w4 trade closes w3 (+100k) → acceleration = +50k.
    let a2 = f.on_event(&opt_trade(T0 + 181 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 50.0, Side::Buy)).unwrap();
    assert!((a2 - 50_000.0).abs() < 1e-6);
}

#[test]
fn ofi_8_whale_threshold_dynamic() {
    // Floor case: p95=$100k → k×p95=300k < floor → threshold $500k;
    // a $600k whale counts, the $100k prints don't.
    let mut floor_case = flow_feature(FlowMetric::WhaleCount, 60 * SEC);
    let mut net_case = flow_feature(FlowMetric::WhaleNet, 60 * SEC);
    for f in [&mut floor_case, &mut net_case] {
        f.on_event(&ticker(T0, "BTC", EXP, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
        for _ in 0..19 {
            f.on_event(&opt_trade(T0 + SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy)); // $100k each
        }
        f.on_event(&opt_trade(T0 + SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 6_000.0, 100.0, Side::Buy)); // $600k
    }
    let count = floor_case.on_event(&opt_trade(T0 + 61 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy)).unwrap();
    assert_eq!(count, 1.0);
    let net = net_case.on_event(&opt_trade(T0 + 61 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy)).unwrap();
    assert!((net - 600_000.0).abs() < 1e-6);

    // K-path case (floor lowered to $10k): 19×$50k + 1×$600k → exact-sort
    // p95 = $50k → threshold = max(10k, 3×50k) = $150k → only the $600k
    // print is a whale.
    let mut params = flow_params();
    params.whale_floor_usd = 10_000.0;
    let mut g = FlowFeature::new(FlowMetric::WhaleCount, "BTC", 60 * SEC, params);
    g.on_event(&ticker(T0, "BTC", EXP, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
    for _ in 0..19 {
        g.on_event(&opt_trade(T0 + SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 500.0, 100.0, Side::Buy)); // $50k
    }
    g.on_event(&opt_trade(T0 + SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 6_000.0, 100.0, Side::Buy)); // $600k
    let c = g.on_event(&opt_trade(T0 + 61 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy)).unwrap();
    assert_eq!(c, 1.0);
}

#[test]
fn ofi_9_deterministic_golden() {
    let replay = || {
        let mut f = flow_feature(FlowMetric::NetPremium, 60 * SEC);
        f.on_event(&ticker(T0, "BTC", EXP, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
        let mut out = Vec::new();
        for i in 0..6u32 {
            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
            let ts = T0 + (i * 61 + 1) as i64 * SEC;
            if let Some(v) = f.on_event(&opt_trade(ts, "BTC", EXP, 100_000.0, OptionKind::Call, 500.0, 100.0, side)) {
                out.push(v);
            }
        }
        assert_eq!(out.len(), 5); // last partial window never emitted
        out
    };
    assert_eq!(format!("{:?}", replay()), format!("{:?}", replay()));
}

#[test]
fn ofi_10_check_config_rejects_unknown_fields() {
    let res: Result<FeaturesConfig, _> = toml::from_str(
        r#"
[options_flow]
underlyings = ["BTC"]
whale_treshold_usd = 5.0
"#,
    );
    assert!(res.is_err()); // typo'd key is an error (deny_unknown_fields)
}

#[test]
fn ofi_12_xdiv_suppressed_single_venue() {
    let mut f = flow_feature(FlowMetric::CrossVenueDivergence, 60 * SEC);
    f.on_event(&ticker(T0, "BTC", EXP, 100_000.0, OptionKind::Call, 0.6, 10.0, 100_000.0, (0.5, 1e-5, 0.0, 0.0)));
    for i in 0..3u32 {
        f.on_event(&opt_trade(T0 + SEC * (i + 1) as i64, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy));
    }
    // Deribit-only: divergence emits NOTHING — never a fake zero (OFI-12).
    assert!(f.on_event(&opt_trade(T0 + 61 * SEC, "BTC", EXP, 100_000.0, OptionKind::Call, 1_000.0, 100.0, Side::Buy)).is_none());
}

#[test]
fn ofi_13_spot_flow_not_duplicated() {
    // Options flow ids must not collide with the spec 004 spot/perp family
    // (cvd.*, whale_print.*, whale.net.*) and must be unique among themselves.
    let cfg: FeaturesConfig = toml::from_str(
        r#"
[options_flow]
underlyings = ["BTC"]
windows_ns = [60000000000]
"#,
    )
    .unwrap();
    let mut e = FeatureEngine::new(SEC);
    mp_features::register_options_families(&mut e, &cfg).unwrap();
    let names: Vec<String> = e.name_map().values().cloned().collect();
    assert!(names.iter().all(|n| !n.starts_with("cvd.")));
    assert!(names.iter().all(|n| !n.starts_with("whale.")));
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "duplicate feature ids registered");
}

#[test]
fn engine_from_config_registers_all_three_option_families() {
    let cfg: FeaturesConfig = toml::from_str(
        r#"
[options_greeks]
underlyings = ["BTC", "ETH"]
[options_iv]
underlyings = ["BTC"]
[options_flow]
underlyings = ["BTC"]
"#,
    )
    .unwrap();
    let mut e = FeatureEngine::new(SEC);
    mp_features::register_options_families(&mut e, &cfg).unwrap();
    let names = e.name_map().values().cloned().collect::<Vec<_>>();
    for expect in [
        "gex.net.btc",
        "gex.net.eth",
        "gex.max_pain.btc",
        "net.delta.eth",
        "vanna.btc",
        "charm.eth",
        "iv.atm.btc",
        "iv.index.btc",
        "iv.skew.btc.rr25",
        "iv.skew.btc.wing25",
        "iv.term.btc.1m",
        "iv.percentile.btc",
        "iv.regime.btc",
        "flow.net_premium.btc.1h",
        "flow.whale.count.btc.24h",
        "flow.by_moneyness.otm.btc.4h",
        "flow.accl.btc.1h",
        "flow.xdiv.btc.24h",
    ] {
        assert!(names.iter().any(|n| n == expect), "missing {expect}");
    }
}

#[test]
fn window_label_helper_matches_feature_ids() {
    use mp_features::options_flow::window_label;
    assert_eq!(window_label(3_600_000_000_000), "1h");
    assert_eq!(window_label(14_400_000_000_000), "4h");
    assert_eq!(window_label(86_400_000_000_000), "24h");
    assert_eq!(window_label(90 * SEC), "90s");
}













