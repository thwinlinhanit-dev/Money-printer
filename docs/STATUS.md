# STATUS — operational truth (generated)

**Regenerate, do not hand-edit:** run `cargo run -p mp-ops -- status`
(read-only), then replace the embedded JSON below — per CLAUDE.md §Docs
moratorium ("one status page"). Last generated: **2026-09-03**.

`backup.last_entry.vps_host` is redacted below (PD-2: production host IPs
never live in the repo — set `MP_VPS_HOST` locally instead).

## Traffic light (as of 2026-09-03)

| Signal | State |
|---|---|
| Trading mode | `sleep` (config-or-default) |
| Latest scorecard | 2026-08-24 — **10 days blind** (D1 breach) |
| VPS drain | **14 files held** (ssh_failed) — P0 item 2 |
| Backup | last good entry 2026-08-19T01:45Z — **15 days stale** (D2) |
| Pipeline log | absent (`data/scorecards/pipeline.log`) |
| Promotion | 7/7 clean-day streak, but burst days present → **NOT promoted** (D3 honest) |
| Kill latch | not latched |

⇒ P0 (`docs/COMPLETION-MASTER-PLAN.md` §P0) remains the only priority. For a
runnable traffic-light on the host, use `ops/scripts/mp_health.ps1` (read-only).

## Embedded JSON (`mp-ops status`)

<!-- MP_OPS_STATUS_JSON -->

```json
{"backup":{"last_entry":{"destination":"C:/mp-backup","dst_delta_bytes":2498604051,"dst_delta_files":93,"elapsed_sec":4547,"full_copy":false,"mtime_utc":"2026-08-16 00:30:02","skip_integrity":false,"src_delta_bytes":2460702356,"src_delta_files":93,"ts_utc":"2026-08-19T01:45:56.2653262Z","vps_host":"<redacted: set MP_VPS_HOST>"},"path":"C:\\mp-backup\\vps-data\\backup_manifest.jsonl","present":true},"compact_zero_row_incidents":{"count_7d":0},"coverage_trend":[{"date":"2026-08-11","recordings":[{"clean":false,"coverage":0.9935014328745372,"symbol":"BTC","venue":"hyperliquid"},{"clean":false,"coverage":0.9934855562823258,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-12","recordings":[{"clean":false,"coverage":0.9568291977402644,"symbol":"BTC","venue":"hyperliquid"},{"clean":false,"coverage":0.9595684807437418,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-13","recordings":[{"clean":true,"coverage":1.0,"symbol":"BTC","venue":"hyperliquid"},{"clean":true,"coverage":1.0,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-14","recordings":[{"clean":false,"coverage":0.9705372896605698,"symbol":"BTC","venue":"hyperliquid"},{"clean":false,"coverage":0.9718183772384742,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-15","recordings":[{"clean":false,"coverage":0.8472346281882088,"symbol":"BTC","venue":"hyperliquid"},{"clean":false,"coverage":0.8419564269124729,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-16","recordings":[{"clean":false,"coverage":0.9931853532782364,"symbol":"BTC","venue":"hyperliquid"},{"clean":true,"coverage":1.0,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-17","recordings":[{"clean":false,"coverage":0.9782478352518588,"symbol":"BTC","venue":"hyperliquid"},{"clean":false,"coverage":0.9782474149587274,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-18","recordings":[{"clean":true,"coverage":0.9981120337933642,"symbol":"BTC","venue":"hyperliquid"},{"clean":true,"coverage":0.9981115359301008,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-19","recordings":[{"clean":true,"coverage":1.0,"symbol":"BTC","venue":"hyperliquid"},{"clean":true,"coverage":1.0,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-20","recordings":[{"clean":true,"coverage":1.0,"symbol":"BTC","venue":"hyperliquid"},{"clean":true,"coverage":1.0,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-21","recordings":[{"clean":true,"coverage":1.0,"symbol":"BTC","venue":"hyperliquid"},{"clean":true,"coverage":1.0,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-22","recordings":[{"clean":true,"coverage":1.0,"symbol":"BTC","venue":"hyperliquid"},{"clean":true,"coverage":1.0,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-23","recordings":[{"clean":true,"coverage":1.0,"symbol":"BTC","venue":"hyperliquid"},{"clean":true,"coverage":1.0,"symbol":"ETH","venue":"hyperliquid"}]},{"date":"2026-08-24","recordings":[{"clean":true,"coverage":1.0,"symbol":"BTC","venue":"hyperliquid"},{"clean":true,"coverage":1.0,"symbol":"ETH","venue":"hyperliquid"}]}],"data_home":{"has_downloads":null,"path":null},"drain_held":{"held_count":14,"present":true},"killswitch":{"file":"C:\\ProgramData\\money-printer\\kill.json","latched":false},"last_scorecard_date":{"date":"2026-08-24","days_since":10},"latest_scorecard":{"date":"2026-08-24","promotable":true,"recordings":[{"blocking_findings":0,"clean":true,"coverage":1.0,"event_count":832136,"stale_bursts":1,"symbol":"BTC","venue":"hyperliquid","worst_gap_ns":0},{"blocking_findings":0,"clean":true,"coverage":1.0,"event_count":555738,"stale_bursts":0,"symbol":"ETH","venue":"hyperliquid","worst_gap_ns":0}]},"mode":"sleep","mode_source":"config-or-default","pipeline":{"path":"data/scorecards/pipeline.log","present":false},"promotion":{"burst_days":[{"date":"2026-08-21","recordings":["hyperliquid:BTC","hyperliquid:ETH"]},{"date":"2026-08-22","recordings":["hyperliquid:BTC","hyperliquid:ETH"]},{"date":"2026-08-23","recordings":["hyperliquid:BTC","hyperliquid:ETH"]},{"date":"2026-08-24","recordings":["hyperliquid:BTC"]}],"consecutive_clean":7,"determinism_failures":[],"determinism_ok":true,"first_failure":"2026-07-18","promoted":false,"required":7,"window_end":null,"window_start":null},"promotion_note":null}
```
