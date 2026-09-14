# oi-purge-v1 — Hypothesis

## Edge: what inefficiency, who pays us and why do they accept the loss?
A large open-interest drop on a price move is the ledger recording forced
deleveraging: positions were closed by margin calls, stop-outs, or partial
liquidations — flow executed by need, not opinion. The deleveraging seller
accepts any price; when the forced flow exhausts (OI stops falling), the
mechanical pressure is gone and price tends to reclaim the ground the panic
traded through. We buy the completion of a long-flush (short the completion
of a short-flush) with a tight invalidation: this is liq-fade-v1's
mechanism transplanted to a data source that hyperliquid actually records
(OI/mark hourly via `activeAssetCtx` — HL has NO liquidation stream, which
is why liq-fade died as n=0 on the HL tape and n=1 quadrants elsewhere).

Trade shape: directional with hard stop. Short horizon (hours), never held
through a second session boundary without re-arm.

## Prior-family evidence (honest, written first)
- liq-fade-v1 KILLED (SWG-8 frozen; ALP-6: reopening needs a new hypothesis
  id — this is it, grounded in a different recorded stream, not a silent
  unkill).
- oi-purge-continuation (backlog row) was NOT GRADABLE: n=1 quadrant-4
  purge in 8 corpus days at ≥1% hourly OI drops. The n was the blocker —
  the corpus has since tripled (bybit continuous 08-25→09-12 with
  `tickers` OI recorded per hour, ~19 days × 3 symbols; HL same window),
  and bybit adds a second venue with an independent OI ledger. The caution
  stands: ≥2% hourly OI drops are rare — if they stay rare at triple the
  data, the honest verdict is still NOT GRADABLE and this stays held.

## Signal decomposition (recorded; queried via `mp-query carry` / `oiwa`)
- `oi_1h` — open interest per venue per hour (bybit `tickers`, HL
  `activeAssetCtx`), USD-denominated (`oi_unit: usd`).
- `oi_chg_1h` = OI_t / OI_{t−1} − 1 (prior closed hour; no lookahead).
- `mark_ret_1h` — mark-to-mark hourly return (same bar convention as the
  08-15 study so results are comparable).
- Quadrants: Q4 = OI ↓ AND price ↓ (long flush) → LONG; Q2 = OI ↓ AND
  price ↑ (short flush) → SHORT.

Entry:
- `oi_chg_1h ≤ −purge_pct` AND `|mark_ret_1h| ≥ move_pct` in the SAME
  direction as the flush side, AND **exhaustion**: OI stop-decline confirmed
  by the NEXT hour (entry executes at hour t+1 open — the flush must have
  stopped falling before we act; we do not catch the knife, liq-fade's
  written lesson).

Exit:
- price reclaims the flush window's midpoint (reversion complete), OR
- adverse excursion ≥ `stop_pct` from entry (hard), OR
- hard time stop 24h.

## Regime dependency: declared_regime + why
`regime.vol = High` (any trend). Purges are definitionally vol events; in
calm regimes the gate never fires (empirically true — that is the n problem).
Portfolio role: episodic right-tail harvest of forced-flow dislocations;
pays when trend and carry both bleed.

## Data gates (written BEFORE any backtest; prefix OPG-n)
- **OPG-1** ≥ 12 qualifying purge events across the recorded corpus at the
  pre-registered thresholds, spanning ≥ 3 distinct calendar weeks AND ≥ 2
  symbols. Fewer ⇒ NOT GRADABLE, stays held (the 08-15 honest precedent).
- **OPG-2** Threshold grid is FIXED pre-registration, not swept to maximize
  results: `purge_pct ∈ {2%, 3%}` × `move_pct ∈ {0.5%, 1.0%}` — 4 configs,
  all reported, none selected post hoc (the orderflow-v1 overfit lesson).
- **OPG-3** Two-venue corroboration when both record the same hour: the
  flush must be visible on bybit OR HL; a single-venue OI artifact (API
  glitch) alone must not qualify. Corroboration rate reported, not enforced.
- Gate order: event study FIRST — CAR[+6h] and CAR[+24h] mark-to-mark (BTC−
  ETH excess cross-checked so market beta is not the "edge"), seeded
  bootstrap CI95, partial windows omitted (SIM-6).

## Falsification (written BEFORE any backtest)
Kill if, with full costs (taker entry+exit + one perp-spread crossing per
side + funding accrued while held — the registered cost model):
- event-study CAR CI95 excludes 0 OPPOSITE the hypothesis at either horizon
  in ≥ 2 of the 4 pre-registered configs (the flush continues, not reverts), OR
- expectancy ≤ 0 in the 2×-cost column in ≥ 3 of 4 configs, OR
- all profitable episodes fall in < 3 calendar weeks (not a harvest), OR
- WF OOS sign flips vs in-sample in ≥ 2 of 3 purged/embargoed windows.
NOT-GRADABLE on OPG-1 ⇒ held, not killed, not silently promoted.
Determinism per spec 018.

## Risks: what breaks it
Deleveraging ≠ sentiment reversal (the flush may be ONE large holder
repricing, not forced flow — OI level context reported per event);
continued-cascade regime (the exhaustion hour is a pause, not a stop — this
is exactly liq-fade's re-acceleration exit, carried over); OI unit/venue
drift (bybit OI in USD of contracts — instrument_master must be checked per
symbol before grading); sparse events make every verdict fragile (hence
OPG-1's floor and the held-not-killed rule).

## Edge results
(none yet — pre-registered 2026-09-13, awaiting OPG-1..3 grading)
