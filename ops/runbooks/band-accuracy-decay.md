# band-accuracy-decay (P3)

The RES-4 whale band-accuracy trend (spec 029 LIQ-6) shows sustained quality
loss: with ≥ 12 graded weeks, the trailing 4-week mean coverage dropped below
half the 12-week mean (baseline ≥ 0.5), or the trailing 4-week mean relative
error more than doubled the 12-week mean (OPS-13, RES-3 semantics).

## Symptoms
- Alert names the metric(s) and weeks: e.g. "coverage trailing 4-wk mean 0.350
  < half of 12-wk mean 0.743 (2026-W29..2026-W32)".
- `research/band_accuracy/band_accuracy.jsonl` shows the recent weeks'
  `mean_relative_error` / `coverage` drifting from the older ones.

## Diagnosis
- Re-run the affected weeks by hand and compare: `research/run_band_accuracy.py
  --log <hl.log> --log <positions.log> --config features.toml` (idempotent —
  an existing `{week}.json` ledger is never rewritten, W-6).
- Look for market-regime causes first (a volatility regime change shifts
  liquidation levels and is expected), then model causes (leverage-tier
  weights out of date — re-run the LIQ-11 calibration), then data causes
  (mp-whale census gaps degrading the pairing sample, LIQ-10 skips).

## Remediation
- P3 FYI: no action required by itself — it is a slow-drift signal, not an
  incident. Log the diagnosis.
- If the estimate is decaying because the leverage-tier assumptions drifted
  from the recorded census: calibrate `[liq_est_bands]` weights via
  `whale_study --leverage-calibration` / `run_calibrate_leverage.py` and
  deploy the new `features.toml` (LIQ-11).
- Do NOT delete or rewrite the trend journal (W-6, append-only); the decay is
  a fact of the record.

## Escalation
Sustained decay across a full 4-week window with no identified regime cause
⇒ surface to the owner with the diagnosis; `liq.est_bands` must clear its
validation grade before any strategy use (LIQ-6, WHL-5).
