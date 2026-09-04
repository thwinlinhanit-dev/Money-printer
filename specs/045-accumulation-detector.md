# Accumulation Detector (spec 045 — ACC)

**Status:** Complete in accumulation.rs

**Purpose**
A compound screener rule that fires only when three independent higher-TF sub-signals co-occur (OI rising + smart-money buying + exchange outflow). This is the first "intelligent" signal that feeds the research pipeline (RES-4 event study).

**Requirements**
- ACC-1: Fires only on simultaneous activation of three legs.
- ACC-2: Observability — input_features() lists every consumed feature.
- ACC-3: Dynamic percentile thresholds (default top 20% OI, top 30% smart flow).
- ACC-4: 4h per-symbol cooldown.
- ACC-5: Regime checks (trend = 0, outflow = 2.0).
- ACC-6: AND logic with explicit sub_signal flags in the hit.
- ACC-7: Per-symbol state (ValueBuffer for rolling percentiles).
- ACC-8: deny_unknown_fields on config.

**Config**
```toml
[accumulation]
oi_delta_feature = "oi.delta.4h"
cooldown_ns = 14400000000000
```

**Acceptance criteria**
1. Fires only on simultaneous activation of three legs (ACC-1).
2. input_features() lists every consumed feature (ACC-2).
3. Dynamic percentile thresholds adapt to data distribution (ACC-3).
4. 4h cooldown prevents duplicate hits per asset (ACC-4).
5. Regime checks enforced (ACC-5).
6. AND logic with explicit sub_signal flags (ACC-6).
7. Per-symbol state maintained via ValueBuffer (ACC-7).
8. Config uses deny_unknown_fields (ACC-8).

**Amendment 2026-09-04 (spec 054, REL-2/REL-6/REL-22)**
- Insufficient history now BLOCKS: a percentile window with fewer than 5
  in-window samples returns `None` and the sub-signal does not fire — the
  legacy `unwrap_or(0.5)` neutral fallback is removed (missing data is never
  scored as median).
- Evidence representation: hits now carry per-leg support in the snapshot
  (`evidence.oi_delta.percentile`, `evidence.oi_delta.samples`,
  `evidence.smart_flow.percentile`, `evidence.smart_flow.samples`,
  `evidence.insufficient_history`) plus the existing `sub_signal.*` and
  `raw.*` fields.
- `AccumulationDetector::quality(symbol, now_ns)` exposes the blocked state
  explicitly (`InsufficientHistory` until both percentile windows have ≥ 5
  samples) so a silent non-firing symbol is diagnosable.
- Strict AND logic (ACC-6) and the cooldown (ACC-4) are unchanged.

**Zero-Cost Mode Behavior**

Under Zero-Cost Mode (`docs/ZERO_COST_MODE.md`), the accumulation detector
degrades gracefully:

- **OI leg:** Always available (Hyperliquid `activeAssetCtx` provides OI)
- **Smart money leg:** Available if whale census (spec 028) is running;
  otherwise the sub-signal is marked inactive
- **Exchange outflow leg:** Requires netflow data (spec 034); if absent,
  the sub-signal is marked inactive

The detector fires only when **all available legs** co-occur (AND logic).
If fewer than 3 legs are active, the detector does not fire — it does not
hallucinate signals from missing data. This is safe: the detector simply
produces fewer hits until more data sources are available.

The `zero_cost_compatible` flag is `true` for this detector because its
core logic (OI + price regime) works with Zero-Cost streams; the exchange
outflow leg is additive, not required.

**Dependencies**
- specs/004-feature-engine.md (FeatureUpdate, TickFeature, BarFeature)
- specs/017-screener.md (ScreenerHit)
- specs/042-wallet-cohort-grading.md (cohort.smart_flow)
- specs/043-cex-flow-velocity.md (netflow.velocity)

**Falsification / Kill conditions**
- Any signal whose last grade has avg_excess <= 0 is automatically demoted or killed.
- Killed signal cannot be resurrected.
- Failed on any acceptance criteria check.
