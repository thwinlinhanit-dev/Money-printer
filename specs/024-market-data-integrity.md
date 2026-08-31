# 024 — Market-Data Integrity Gate

## Purpose

Make a raw market-data recording attributable, auditable, and safe to promote
into research.  The system is a market-data integrity system first: a strategy
may only consume a recording that has passed this gate.

## Scope

In: one venue/symbol per raw log, event-level provenance, raw-log audit,
quarantine classification, compaction refusal, and a daily scorecard.  Out:
authenticated trading, strategy changes, and long-running hosting policy.

## Requirements

- **INT-1** Every raw event MUST retain its venue/symbol envelope and transport
  provenance: stream, exact subscription, connection id, and snapshot source.
- **INT-2** A raw log MUST contain one venue and one symbol only.  Mixed,
  unknown, malformed, or legacy-unattributable logs are quarantined.
- **INT-3** The audit MUST report event counts, first/last receive timestamps,
  coverage, gaps, stale periods, mismatches, symbol-table validity, and
  provenance failures.
- **INT-4** Compaction MUST refuse any audit result that is not clean.
- **INT-5** The daily scorecard MUST report each required venue/symbol/stream
  and only mark a day promotable when every required recording is clean.

## Acceptance criteria

- [x] `int_1_event_provenance_roundtrips` proves provenance is event-level.
- [x] `int_2_mixed_log_is_quarantined` proves a venue/symbol mismatch fails.
- [x] `int_3_audit_reports_gap_and_staleness` proves coverage diagnostics.
- [x] `int_4_compaction_refuses_quarantined_log` proves contaminated data never
  reaches cold storage.
- [x] `int_5_scorecard_requires_every_recording_clean` proves promotion is
  based on the required matrix, not process uptime.

## Decisions

- 2026-07-29: Existing schema-v1 raw logs are deliberately classified as
  legacy/unattributable and quarantined.  They are never silently promoted.
- 2026-08-03 (audit follow-up): schema-v1 logs became readable again via a
  backward-compatible decode path (`mp_core::log::LogReader` decodes the
  pre-provenance envelope layout and normalizes it to the current envelope
  with synthetic provenance).  This does NOT change the INT-1 verdict:
  synthetic provenance fails `missing_provenance`, so legacy recordings are
  readable for research/replay but still never promoted to cold.  A
  migration tool (`mp-storage` `mp-migrate` bin + `migrate::migrate_log`)
  rewrites schema-1 logs to current-schema files in a separate output
  directory, W-6 style (write-new + verify, originals untouched).
- 2026-07-29: The collector process remains one `(venue, symbol)` pair.  Cross
  venue observations are separate recordings and are merged only at replay.
- 2026-08-03: INT-4 is enforced at the compaction boundary via
  `compact_day_verified` (returns `StorageError::Refused` and writes nothing
  when the raw log's audit is not clean).  `compact_day` stays the pure
  mechanics entry point for already-verified paths; promotion must go through
  the verified gate.
- 2026-08-04: Raw log appends are recv-monotonic, enforced at the write
  boundary by `mp_collectors::monotonicize` (each batch is sorted by
  `(recv_ts_ns, stream_seq)` and stragglers clamped to the running clock;
  EVT-5/STO-4 convention).  Root cause: REST-injected events regressed the
  recv clock by their HTTP round-trip — the 30s open-interest poll is stamped
  *after* its ~1.5 s fetch but appended *before* the WS frames read during it
  (and depth reseeds are stamped *before* their fetch).  That produced
  `recv_time_reversal` on every recording and made the promotion gate
  unreachable.  WS frames are already monotonic (single FIFO reader stamps at
  socket read), so the clamp only touches REST-injected stragglers.
- 2026-08-04: `recv_time_reversal` is a non-blocking warning (`[warn]` in
  `mp-audit`): it records pure arrival-order jitter with zero frame loss.
  Cleanliness = no *blocking* findings (`is_blocking_finding`); every
  data-loss / unattributable code — `legacy_or_malformed`, `unreadable_log`,
  `empty_log`, `missing_stream`, `missing_provenance`, `venue_mismatch`,
  `symbol_mismatch`, `invalid_symbol_table`, `missing_snapshot_source`,
  `sequence_gap`, `backpressure_loss`, `coverage_gap`, `stale_stream` —
  remains a blocker.  Consumers of an audit must judge cleanliness by the
  `clean` flag / `is_blocking_finding`, never by "findings non-empty".
- 2026-08-12 (supersedes part of the above): the Phase-0 gate is now the
  ROADMAP's numeric bar — **`coverage ≥ 0.995`** (`MIN_COVERAGE`) with no
  identity/provenance/parse findings and no loss the coverage number cannot
  see.  `stale_stream` (a COL-2 recovery signal — the collector already
  reconnected) and `coverage_gap` (its severity IS the aggregate coverage
  number) are warnings, and `low_coverage` is the blocker that carries the
  numeric verdict.  Rationale from live data: on 2026-08-09 a single ~138s
  recv hole plus ~400 stale events left coverage at 0.9984 — above the
  0.995 criterion yet DIRTY under the old any-blocking-finding gate; the
  gate was stricter than its own requirement and structurally unmeetable on
  any network.  `sequence_gap` and `backpressure_loss` remain blockers:
  they are real loss the recv-clock coverage number cannot see.  Margin
  fields keep every verdict self-explaining: `worst_gap_ns` (largest hole),
  `stale_bursts` (stale events grouped into windows, 90s gap — same
  constant the profiler uses), both now in the audit JSON and scorecard.
  Consumers must still judge cleanliness by the `clean` flag /
  `is_blocking_finding`, never by "findings non-empty".
- 2026-08-12 (ENFORCED — implemented in `mp_storage::promotion`
  `check_promotion` and `mp-ops promote`): the **Phase-0 promotion gate**
  adds one window-level condition: the qualifying 7-day window must have
  **zero `stale_bursts`** across every required recording.  This is a
  PROMOTION condition, NOT a per-day veto: a day with a stale burst still
  audits and still counts toward the streak (the numeric bar above is
  unchanged — the 2026-08-12 tolerance semantics hold), but `PROMOTED`
  additionally requires the window's `stale_bursts` total to be 0, on
  every required venue:symbol, for every day in the window.  Rationale:
  the VPS move's entire premise is that datacenter egress eliminates the
  ~3h20m-periodic bursts that dirtied the Windows host (08-08 0.989,
  08-10 0.983, 08-11 0.993 vs 08-09 0.9984 — the bursts are a real path
  hole, not a collector defect); the runbook's §5 already names "zero
  `stale_bursts` across all 7 days" as the root-cause proof, and this
  makes that proof enforced rather than aspirational — a 7/7 `PROMOTED`
  on a host that still bursts every ~3h20m would validate the wrong
  hypothesis.  The condition is achievable by design on the target host
  (that is the claim being validated), and a failed verdict stays
  informative: the streak counter keeps advancing, the `why` names the
  burst days, and the §6 A-B decides the next action (do not retire the
  Windows recorder while the hypothesis is unproven).  Implementation:
  `DailyScorecard` carries per-recording `stale_bursts` counts
  (`recording_bursts`, populated by `scorecard()` and by `mp-ops promote`
  scoped to the `--required` set — a day's extra recordings never veto a
  window covering the required corpus); the verdict exposes `burst_days`
  (date + the venue:symbol recordings that carried bursts) as the `why`
  when a full streak is held back; and the qualifying window is the
  latest burst-free run of `REQUIRED_CONSECUTIVE_CLEAN_DAYS` inside the
  streak — on equal-length runs the most recent is preferred, so a fresh
  burst-free tail promotes immediately instead of being masked by an old
  bursty streak.  Scoped to the Phase-0 gate (the first promotion that
  qualifies the corpus); after it, the ongoing daily gate remains the
  numeric bar and `stale_bursts` / `worst_gap_ns` stay margins in every
  scorecard.  `worst_gap_ns` is NOT part of the condition: sub-tolerance
  holes are the designed coverage trade (the 0.995 bar absorbs them),
  while the periodic stale burst is the signature that distinguishes a
  clean path from a degraded one.  Evidence the two conditions measure
  different degradations (2026-08-12 survey, `ops/scripts/margin_correlation.py`
  over the 8 real hyperliquid recordings 08-08..08-11): `coverage` vs
  `worst_gap_ns` r = −0.855 (p < 0.05 — the numeric bar is driven by the
  largest hole, as designed), but `worst_gap_ns` vs `stale_bursts` r = +0.448
  and `coverage` vs `stale_bursts` r = −0.392, both n.s.  The strongest
  pairwise evidence is non-monotonic: the only CLEAN day (08-09) had the
  smallest gap (136s) and the most bursts (11–15), while 08-11 (dirty, gap
  239s) had just 2 bursts.  A clean day can be bursty and a dirty day can be
  burst-light — the window condition is not redundant with the numeric bar
  (it vetoes days the bar passes, e.g. 08-09) and it is not a proxy for it
  (it lets days through the bar rejects, e.g. 08-11).  Caveat: n = 8 is
  small; re-run `margin_correlation.py` as VPS days accumulate to sharpen
  both correlations.
- 2026-08-04: `mp-audit --json` emits a lightweight per-recording summary
  (clean flag, counts, per-code findings histogram) instead of the full
  findings Vec — legacy files hold millions of findings, and serializing them
  ballooned `--json` to multi-GB output.  Text mode keeps full per-finding
  detail.
- 2026-08-04: the daily integrity pipeline runs on the native Windows host as
  a Scheduled Task (`MoneyPrinterDailyPipeline`, 00:05 UTC — right after the
  collector's UTC-midnight log rotation, so yesterday's file is closed and
  auditable).  `ops/scripts/daily_pipeline.ps1` is the Windows port of
  `ops/scripts/daily_maintenance.sh`: it archives the daily scorecard to
  `data/scorecards/<date>.json`, exits 1 (Task Scheduler flags the run) when
  the day is not promotable, compacts through the INT-4 verified gate when it
  is, and prints the promotion streak verdict (longest run of promotable
  scorecards).  The 7-day streak is thus tracked from scorecard history, and
  the promotion gate advances automatically once clean days accumulate.  A
  DST/clock-drift guard no-ops runs outside the 00:00–02:00 UTC window;
  `-RegisterTask` recomputes the local wall-clock of 00:05 UTC.
- 2026-08-04 (incident): from this network, `fstream.binance.com` (futures)
  delivers ONLY orderbook streams — `depth@100ms` and `bookTicker` flow, while
  `aggTrade`, `markPrice@1s`, `ticker`, and `miniTicker` are silently dropped
  (verified with a raw WS probe: combined-stream URL, single-stream path,
  SUBSCRIBE method, multiple symbols — identical result; spot
  `stream.binance.com` delivers `aggTrade` normally).  Not a collector defect:
  subscription URLs are correct.  Regional egress restriction on futures
  non-book streams.  Consequence: recordings since 2026-08-03 16:15 UTC are
  book-only; they fail the required-streams check and are NOT promotable,
  which is correct.  Recovery: change egress (VPN/proxy in an allowed region)
  and restart collectors; verify with a raw probe that `aggTrade` frames
  arrive before trusting the recording.
- 2026-08-04 (incident follow-up, verified evidence): the futures-WS filter is
  at Binance's edge for this egress IP, not a network/DNS block.
  `fstream.binance.com` TCP-connects (101 Switching Protocols) but only
  `depth` frames flow; `aggTrade`/`markPrice`/`forceOrder` are silently
  dropped.  The documented alternative futures host `fstream.binance.vision`
  is **globally NXDOMAIN** (even via 8.8.8.8/1.1.1.1) — that mirror is dead;
  `data-stream.binance.vision` (spot) resolves but fails the TLS handshake
  from this network.  `fapi.binance.com` REST works (trades, premiumIndex,
  OI all return) — only the WS non-book streams are filtered.  Bybit, OKX and
  Hyperliquid WS all connect normally from this network.
- 2026-08-04 (fix): the collector WS transport now supports an egress proxy —
  `proxy = "http://host:port"` (HTTP CONNECT) or `"socks5://host:port"` in
  the collector config, or the `MP_WS_PROXY` env var (overrides config on
  both the `--config` and flag paths).  The proxy only carries bytes: TLS is
  terminated against the venue with webpki roots, never the proxy.  Verified
  with mock-proxy unit tests (`proxy_http_connect_accepts_200_and_rejects_403`,
  `proxy_socks5_handshake_completes`).  **Runbook:** set up a proxy/VPN whose
  egress IP is in an allowed region, set `MP_WS_PROXY`, restart the collectors
  (Stop-ScheduledTask MoneyPrinterCollectorsWatchdog → Start-ScheduledTask),
  then verify with the raw probe that `aggTrade`/`markPrice`/`forceOrder`
  frames arrive before trusting new recordings.
- 2026-08-04 (interim mitigation, COL-25/26/27): while the WS trade stream is
  filtered, the collector can ingest trades from `GET /fapi/v1/aggTrades`
  (REST works from this network) with a loss-free `fromId` watermark
  (`trade_source = "rest"` in config; WS aggTrade frames are suppressed at
  the normalizer, watermark jumps surface as `Status::GapDetected`).  This
  keeps the trade stream populated but does NOT restore the WS `markPrice`/
  `forceOrder` streams — those still need the proxy fix above.
- 2026-08-04 (interim mitigation, COL-28): the same REST path now closes the
  mark/funding gap.  `mark_source = "rest"` (config or `--mark-source`)
  polls `GET /fapi/v1/premiumIndex` every 15s and injects `MarkPrice` +
  `Funding` envelopes that are byte-identical to the WS `markPriceUpdate`
  branch (mark, index, lastFundingRate, nextFundingTime; interval default
  8h — the venue carries no interval on either payload).  WS markPriceUpdate
  frames are suppressed at the normalizer so a later-restored WS stream can
  never double-record.  Independent of `trade_source` so each stream can be
  restored separately.  Verified live: BTCUSDT/ETHUSDT recordings now show
  `mark_price` + `funding` streams (audit `streams` map).  The proxy fix
  still matters for full fidelity — 1s WS mark cadence, `forceOrder`
  liquidations, and the other blocked streams — but the carry-v1 funding
  dataset no longer depends on it.
- 2026-08-04: `mp-audit --json` per-recording summaries now include the
  `streams` map (per-stream event counts) so operators can verify a stream
  is populated without a separate reader.
- 2026-08-06 (re-verified, filter STILL active): direct raw probe from the
  host (`node ops/scripts/ws_probe.mjs` — node's native WebSocket; Python is
  absent from the host and a PowerShell/.NET ClientWebSocket probe undercounts
  to zero on the same connection) over a 20s window shows
  `depth@100ms`=190, `bookTicker`=729, and `aggTrade`=0, `markPrice@1s`=0,
  `forceOrder`=0 — fstream still drops the non-book streams from this egress.
  The 2026-08-05 scorecard confirms the consequence: `streams` map populated
  only via the REST mitigations (trade=741858, mark_price=4488, funding=4488,
  book=665474) and the day is NOT promotable — `sequence_gap` ×57,
  `stale_stream` ×1, `coverage_gap` ×1 (the 15s REST mark poll cannot hold the
  1s WS cadence). `forceOrder` has no REST fallback, so its appearance in the
  audit `streams` map is the honest post-fix signal that WS non-book streams
  flow again. Fix remains the egress proxy (`MP_WS_PROXY` / config `proxy`,
  transport shipped 08-04); runbook: `ops/runbooks/ws-egress-filter.md`,
  probe: `ops/scripts/ws_probe.mjs`.
- 2026-08-13 (COL-29): the `Liquidation` event gets a REAL source, and the
  audit gate judges its provenance.  Ground truth verified live on this
  egress: Binance's public liquidation paths are ALL dead — the WS
  `forceOrder` stream is dropped (2026-08-06 probe) AND `GET
  /fapi/v1/allForceOrders` is a USER_DATA endpoint, not public market data:
  it 404s without credentials while every public fapi endpoint
  (aggTrades/premiumIndex/openInterest) answers.  The credential-free live
  source is **Bybit's public `allLiquidation.`/`liquidation.` WS topics**
  (egress-compatible per 08-08 probe; normalizer + collector subscription
  already existed per spec 002 — now pinned by `col_29_bybit_*` tests and
  documented in `collectors/bybit-btcusdt.example.toml`).  For Binance
  specifically, `liq_source = "rest"` (config or `--liq-source`;
  independent of `trade_source`/`mark_source`) polls the USER_DATA endpoint
  every 10s with an update-time `startTime` resume point and order-id dedup,
  emitting `Liquidation` envelopes byte-identical to the WS `forceOrder`
  branch (avgPrice-or-price, executedQty-or-origQty, `S` side, `updateTime`
  as the exchange timestamp); only `FILLED` orders are kept (an
  unfilled/cancelled force order never closed a position).  The leg is
  signed (HMAC-SHA256, `sign_binance_query`, RFC 4231-vector-pinned) and
  **dead-until-creds**: without `MP_BINANCE_API_KEY`/`MP_BINANCE_API_SECRET`
  the collector refuses to start — never a silent 404 loop (p1-webhook
  idiom).  WS forceOrder frames are suppressed at the normalizer when REST
  is the source (same single-source rule as COL-27/28).  Unlike `aggTrades`,
  `orderId` is shared with regular orders, so gaps carry NO meaning: the
  poller dedups by id and never invents a `sequence_gap` (the endpoint's
  honest loss signal is simply an empty page, which is normal between
  liquidations).  The audit gate now accepts venue-scoped requirements —
  `--require-stream binance:liquidation` (also `bybit:liquidation`) — so a
  Binance/Bybit recording must show the `liquidation` stream every recorded
  day without imposing it on venues that have no native liq stream
  (Hyperliquid; its liquidation data comes from the on-chain whale census,
  spec 028).  Verified: 9 `col_29_*` unit tests (parse, FILLED filter,
  watermark dedup, REST==WS body-equality, suppression, Bybit topic shapes,
  signature vector) + mp-ops venue-scoping test; gate wiring in
  `daily_pipeline.ps1` and `daily_maintenance.sh`.
- 2026-08-25 (gate-integrity plausibility guard — incident 2026-08-22
  follow-up): a scorecard whose **every** required recording audits to
  `event_count == 0` is NOT a dirty day; it means the gate read nothing
  (raw sources never drained, or a reader/writer schema split like the
  Aug-18 deploy that blinded the VPS gate 08-19..21). `mp-ops scorecard`
  now REFUSES such days: loud stderr + exit 2, no verdict JSON archived
  (`--allow-all-zero` is the explicit operator escape hatch). The missed
  archive surfaces through the existing dead-man (`pipeline-stale`,
  OPS-17) as a P1 instead of silently eating the streak. Supporting
  changes: `audit_raw_log` names a missing day-file `recording_missing`
  (blocking, distinct from `unreadable_log`), the promote invariant
  comment tracks the new code name, and the deploy pair-smoke lives in
  `ops/scripts/smoke_reader_writer.sh` (run after ANY partial-binary
  deploy; requires today's live logs to decode with real counts).
  Regression tests: `scorecard_all_zero_refuses_verdict_unless_allowed`,
  `scorecard_decodes_schema_current_writer_events` (mp-ops),
  `int_recording_missing_is_named_and_blocking` (mp-storage).
- 2026-08-31 (Zero-Cost Mode — lowered gate): under Zero-Cost Mode
  (`docs/ZERO_COST_MODE.md`), the Phase-0 gate is relaxed:
  - Required streams: `trade funding mark_price open_interest` (no `book`)
  - Coverage threshold: >= 0.95 (down from 0.995)
  - Stale bursts: warnings only, not a promotion blocker
  - Full book absence: expected, not a dirty finding
  - Promotion streak: 14 consecutive clean days (up from 7)
  - Determinism check: still required on the decision path
  Rationale: with $0 budget and free-tier constraints, the system must be
  able to promote without full book data. The longer streak compensates for
  the lower per-day bar. Zero-Cost Mode is activated by
  `--zero-cost-mode` flag on `mp-ops scorecard`/`promote`, or by setting
  `mode = "zero-cost"` in the ops config.
- 2026-08-25 (proactive WS connection rotation — Hyperliquid venue-TTL
  mitigation): the Phase-0 promotion amendment requires **zero
  `stale_bursts`** across the qualifying window, and every burst since
  08-21 traces to one mechanism: Hyperliquid's edge caps each WebSocket's
  *total lifetime* at ~2h48m–2h57m regardless of traffic (measured across
  08-22..25 from VPS journals — BTC/ETH processes disconnect on independent
  ~2h50m phase-offset schedules with client pings flowing every 10s, so it
  is neither idle timeout nor host/network events). Most TTL kills recover
  sub-second with no finding, but two aggravating modes dirtied days: a
  silent stall before COL-2 fires (08-22 10:13 BTC: 29.6s) and a
  server-side reconnect-refusal loop (08-23 08:17:43–47: ~15 resets in 4s).
  Fix: `WsEndpoint::max_connection_age` (default `None`) makes the
  transport close its own connection voluntarily at age + ≤8% jitter
  (SplitMix64, CONV-11; jitter desynchronizes sibling collectors so they
  never rotate in lockstep). The collector sees an ordinary
  `Disconnected`, reconnects on its existing 250ms full-jitter backoff,
  re-subscribes and re-seeds the book — a scheduled non-event instead of a
  surprise kill. `mp-collector` sets this to **2h15m** for hyperliquid
  only (≥33 min under the observed minimum kill); other venues keep
  live-until-closed until their own TTL is measured. COL-2 remains the
  backstop. Tests: `ws_proactive_rotation_ends_connection_at_max_age`,
  `rotation_delay_is_age_plus_bounded_jitter` (mp-collectors, live-ws).
