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
- Bucket size: ATR-based (default 0.5 ATR)
- Output: POC, VAH, VAL as separate features

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
