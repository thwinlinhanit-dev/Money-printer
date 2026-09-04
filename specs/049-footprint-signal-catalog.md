# 049 — Footprint Signal Catalog

**Status:** Draft

**Purpose**  
Register footprint-derived signals as first-class TickFeature/BarFeature so they appear in the catalog and can be subscribed to by strategies.

**Signals**
- `footprint.cvd.{venue}` — Cumulative Volume Delta per venue (already implemented as `cvd.{venue}`)
- `footprint.delta.{tf}.{bucket}` — Net bid/ask volume inside candle per size bucket (already implemented)
- `footprint.imb.{tf}.{bucket}` — Imbalance ratio per size bucket (already implemented)
- `footprint.volume.bubble.{tf}` — Real volume overlay: total volume per bar with percentile ranking
- `footprint.market.profile.{tf}` — Market Profile: POC (Point of Control) + VAH (Value Area High) + VAL (Value Area Low)

**Registration**  
Register as global tick features that compute on every trade and emit on bar close.

**Design**

### footprint.volume.bubble
Tracks total volume per bar and emits the volume percentile rank relative to a rolling window. Useful for identifying volume climaxes and dry-ups.

```
volume_percentile = rank(current_volume, rolling_window) / window_size * 100
```

- Window: configurable (default 100 bars)
- Output: 0-100 percentile rank
- High values (>80) = volume climax
- Low values (<20) = volume dry-up

### footprint.market.profile
Computes volume-weighted price distribution for a bar window and extracts:
- **POC** (Point of Control): price level with highest volume
- **VAH** (Value Area High): upper boundary of 70% volume
- **VAL** (Value Area Low): lower boundary of 70% volume

```
For each bar in window:
  distribute volume across price range (OHLC)
  find price level with max volume = POC
  find price range containing 70% volume = [VAL, VAH]
```

- Window: configurable (default 50 bars)
- Bucket size: absolute price width (default 0.5 price units; amended
  2026-09-02, A-6 audit — the implementation uses the configured value
  directly as the bucket's price width and never computes an ATR, so the
  config key is `market_profile_bucket_width`)
- Output: POC, VAH, VAL as separate features
- POC tie-break: deterministic (PD-3) — on equal bucket volumes the lowest
  bucket wins (A-5 audit 2026-09-02; hardened 2026-09-04, spec 054 REL-21,
  to `total_cmp` so non-finite values can never resolve as a silent `Equal`;
  VAH/VAL use the same deterministic rule)
- **Approximation (spec 054 REL-20):** this is a BAR-BASED market profile.
  Each bar's entire volume is placed in ONE bucket at the bar midpoint
  `(high+low)/2`; intra-bar volume distribution is invisible and wide bars
  distort the profile. It is an approximation of the true POC/VAH/VAL and
  must never be treated as an L2-equivalent footprint. Trade-level footprint
  requires full L2 data and is out of scope.

**Tests**
- fp_1: volume.bubble emits correct percentile rank
- fp_2: market.profile POC matches highest-volume price level
- fp_3: market.profile VAH/VAL contain 70% of volume
- fp_4: engine_from_config registers all footprint signals when enabled
- fp_5: footprint signals appear in signal catalog

**Acceptance criteria**
- [ ] All 5 footprint signals registered as TickFeature/BarFeature
- [ ] Config section `[footprint]` with `enabled` flag
- [ ] Features emit on bar close (no intra-bar repaint)
- [ ] All signals testable via `cargo test -p mp-features`

**References**
- spec 004 §Order flow (footprint delta/imbalance)
- spec 025 signal catalog (registration)
