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

**Dependencies**
- specs/004-feature-engine.md (FeatureUpdate, TickFeature, BarFeature)
- specs/017-screener.md (ScreenerHit)
- specs/042-wallet-cohort-grading.md (cohort.smart_flow)
- specs/043-cex-flow-velocity.md (netflow.velocity)

**Falsification / Kill conditions**
- Any signal whose last grade has avg_excess <= 0 is automatically demoted or killed.
- Killed signal cannot be resurrected.
- Failed on any acceptance criteria check.
