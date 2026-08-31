# Signal Catalog + Footprint Accumulation Signals (spec 025)

**Status:** Complete — all acceptance criteria implemented and passing in the branch.

**Purpose**
A versioned, lifecycle-managed catalog of order-flow and derivatives signals that can be consumed by the feature engine, screeners, research notebooks, and LLM briefs. The catalog applies the same funnel discipline as `strategies/funnel.rs` (Hypothesis → Tested → Graded → Deployed, automatic decay, human click only for live promotion) to *features* rather than strategies (SIG-1).

**Motivation (why this spec exists)**
- Prevents schema fragmentation (exactly the iCrypto problem).
- Every signal has a written hypothesis written *before* any evidence.
- Decay re-testing is automatic and deterministic (mirrors RES-3).
- Higher-TF footprint signals (daily/weekly accumulation/distribution) are first-class citizens.

**Scope**
- Core catalog + lifecycle (SIG-1 to SIG-5).
- Footprint-specific signals: `footprint.cvd`, `footprint.delta`, `footprint.imb`, `footprint.volume.bubble`, `footprint.market.profile`.
- Accumulation detector (ACC-1) that consumes catalog signals.
- Integration with feature engine (FEA-4, FEA-20) and bar builder (BAR-1).

**Dependencies**
- specs/001-event-schema.md (EventEnvelope, MarketEvent::Trade/BarSnapshot)
- specs/004-feature-engine.md (FeatureUpdate, TickFeature, BarFeature)
- specs/017-screener.md (ScreenerHit)
- specs/003-storage.md (Parquet manifests for signal history)

**Decisions**
- SIG-1: All signals must have a non-empty hypothesis written before registration (no "I think this works").
- SIG-2: Promotion to Graded requires min_n samples + positive avg_excess.
- SIG-3: Decay detection uses trailing 12-week mean vs 4-week mean (exact formula in SignalRecord::detect_decay).
- SIG-4: Kill requires full justification (autopsy artifact).
- SIG-5: Deployed stage requires human click (agents never promote).
- Accumulation detector (ACC-6) uses AND logic across three independent sub-signals with percentile thresholds.
- All timestamps are nanoseconds UTC; no wall-clock in decision paths.

**Zero-Cost Compatibility**

Every signal in the catalog carries a `zero_cost_compatible` flag:

- `true`: signal can be computed from trades + bars only (no full book needed)
- `false`: signal requires full L2 book depth

Under Zero-Cost Mode (`docs/ZERO_COST_MODE.md`), only `zero_cost_compatible
= true` signals are registered. Signals requiring full book are disabled.

Compatible signals: `footprint.cvd`, `footprint.delta`, `footprint.imb`,
`footprint.volume.bubble`, `footprint.market.profile`, all swing features
(realized_vol, trend_strength, value_area, VWAP, ATR, sweep), climax
variants.

Incompatible signals: any signal requiring multi-level book depth data.

**Open questions**
None — all resolved in the code.

**Acceptance criteria** (every one has an automated test)
1. Register signal with empty hypothesis fails (SIG-1).
2. Promotion only with sufficient samples and positive edge (SIG-2).
3. Graded signal decays automatically after 12-week decay test (SIG-3).
4. Kill requires non-empty justification; cannot resurrect (SIG-4).
5. Deployed stage requires human click (agents refuse) (SIG-5).
6. Footprint signals are registered as TickFeature + BarFeature factories.
7. AccumulationDetector produces ScreenerHit only on co-occurrence of three sub-signals with cooldown.
8. Catalog serializes to JSON and deserializes losslessly.
9. Every signal has a unique id and params_hash for compatibility.
10. Decay re-test interval defaults to 30 days (matches strategies funnel).

**Tests** (all in features/src/signal_catalog.rs and accumulation.rs tests)
- sig_1_register_requires_hypothesis
- sig_2_grade_promotes_only_with_evidence
- sig_3_decay_re_test
- sig_4_kill_requires_justification
- sig_5_deployed_needs_human
- acc_1_three_sub_signals_co_occurrence
- acc_4_cooldown_prevents_duplicates
- acc_6_percentile_thresholds

**Falsification / Kill conditions**
- Any signal whose last grade has avg_excess <= 0 is automatically demoted or killed.
- Killed signal cannot be resurrected.

**Next phase**
Upgrade to warm query layer (TapRooT) so signals can be queried against recorded history.
