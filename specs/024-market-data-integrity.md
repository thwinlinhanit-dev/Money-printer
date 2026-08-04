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
  remains a blocker.
