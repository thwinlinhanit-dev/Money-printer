# 003 — Storage: Tiers, Parquet Layout, Quality Manifests

## Purpose
Turn the event stream into a permanent, queryable, quality-tracked dataset.
Cold Parquet is the research substrate; quality manifests are what make
backtests trustworthy (a gap you know about is data; a gap you don't is poison).

## Scope
In: cold Parquet layout, compactor, quality manifests, warm store (ClickHouse),
retention. Out: event-log format (001), feature materialization (004).

## Design

```
event logs (001) ──▶ compactor (daily job) ──▶ Parquet cold store + manifest
                                          └──▶ ClickHouse warm store (optional insert)
```

### Cold store layout (local disk or S3-compatible)
```
cold/
  trades/venue={v}/symbol={s}/date={YYYY-MM-DD}/part-000.parquet
  book_deltas/venue=.../     (same partitioning)
  book_snapshots/...
  funding/...  mark_price/...  open_interest/...  liquidations/...  status/...
  manifests/venue={v}/date={d}.json
symbols/symbols.parquet          (SCD2: valid_from/valid_to per row)
```
Parquet: zstd level 3, row group ≈ 128MB, sorted by `(symbol, recv_ts_ns)`,
column names exactly matching spec 001 field names.

### Quality manifest (per venue/date)
```json
{ "venue": "bybit", "date": "2026-07-10", "schema_ver": 1,
  "streams": { "trades:BTCUSDT": {
      "events": 1234567, "first_ts_ns": ..., "last_ts_ns": ...,
      "gaps": [{"from_ns":..., "to_ns":..., "kind":"disconnect|overrun|venue"}],
      "coverage": 0.9987, "sampled": false } },
  "compactor_version": "git-sha", "created_ts_ns": ... }
```
`coverage` = fraction of the UTC day not inside a gap. **Every research/sim
read path MUST consult manifests** (SIM-6 depends on this).

## Requirements
- **STO-1** Compactor MUST convert closed (previous-UTC-day) event logs to the
  Parquet layout above, idempotently: re-running produces byte-identical
  files or skips (content hash check).
- **STO-2** Compactor MUST derive the quality manifest from `Status` events +
  observed inter-event intervals; a book stream with unresolved `GapDetected`
  and no following `GapResync` snapshot marks the remainder of day gapped.
- **STO-3** Original event logs MUST NOT be deleted by the compactor (W-6);
  a separate `prune` command (human-run) deletes logs older than N days ONLY
  after verifying Parquet + manifest exist and row counts match.
- **STO-4** A `Dataset` reader API (Rust + Python via the same Parquet) MUST
  stream events for (venues, symbols, time range) in global
  `(recv_ts_ns, stream_seq)` order — the sim's input (SIM-1).
- **STO-5** Reader MUST expose per-range quality: `coverage(range) -> f64` and
  `gaps(range) -> Vec<Gap>` from manifests, without scanning data files.
- **STO-6** Warm store (ClickHouse) is OPTIONAL in v1; if enabled, the
  compactor inserts the same rows; schema mirrors Parquet; TTL 60 days.
  Nothing on a decision path may depend on ClickHouse.
- **STO-7** Disk-budget watchdog: when the data volume exceeds
  `max_disk_pct` (default 85%), alert (spec 009) — never auto-delete.
- **STO-8** Every Parquet file footer MUST carry KV metadata:
  `schema_ver, compactor_version (git sha), source_log_hash`.
- **STO-9** Symbol metadata MUST be persisted as SCD2 (`valid_from_ns`,
  `valid_to_ns`) so historical reads resolve the metadata as-of the event time
  (tick sizes change; delistings happen).

## Acceptance criteria
- [ ] Round-trip: synthetic event log → compactor → Dataset reader yields identical event sequence (STO-1, STO-4).
- [ ] Idempotency: run compactor twice, second run is a no-op (STO-1).
- [ ] Manifest math: injected disconnect + resync in fixture log produces expected gap list and coverage to 6 decimals (STO-2).
- [ ] Prune refuses to delete when row counts mismatch (STO-3).
- [ ] As-of symbol lookup returns the correct tick_size across a change boundary (STO-9).

## Decisions
- 2026-07-10: Parquet is canonical; ClickHouse is a disposable acceleration
  layer. Local disk first; S3 path support behind the same API.

## Open questions
- Off-site backup cadence for `cold/` — needs owner's storage budget decision.
