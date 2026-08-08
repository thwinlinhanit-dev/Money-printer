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
