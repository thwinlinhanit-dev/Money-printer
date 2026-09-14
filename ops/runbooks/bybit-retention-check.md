# bybit-retention-check (P1/P2)

Fired nightly (02:15 UTC, Task Scheduler `MoneyPrinterBybitRetentionCheck`)
by `ops/scripts/bybit_retention_check.ps1` when any bybit day stops
completing the W-6 retention chain. The check AUTO-TRACKS every bybit day
(present raws + journaled history; window = [earliest bybit day, today
UTC]) — no fixed date window to roll over; days 09-07+ are watched as they
land. Explicit `-EarliestDate/-LatestDate` override the window for
tests/investigations:

> compact (INT-4 audit gate) → cold proof (manifest + Parquet) → `mp-ops prune`
> (hash-verified, journaled to `data/retention_delete_manifest.jsonl`)

Windows has no `ZERO_COST=1`, so the pipeline's retention block never runs
here — nothing auto-prunes. The check is the forcing function: it reports
nightly which crossed days are waiting on the chain and which never
compacted at all.

## Severity

- **P2 (WARN)** — a crossed day is COMPACTED (proof verified) but prune has
  not run yet; or a day within the trailing absent-horizon (default
  2×retention, 28 days) is absent from both raw and journal (deleted
  outside the journaled gate / never landed). Action: run the printed
  `mp-ops prune` command, or investigate the absent day.
- **P1 (CRIT)** — a crossed day was NEVER compacted and is not
  acknowledged: the retention chain is stalled for it.
- **ACKED (silent, counted)** — the day has an entry in
  `data/bybit_retention_acks.jsonl` (append-only, added via
  `bybit_retention_check.ps1 -Ack YYYY-MM-DD -Reason "..."`): the owner
  documented why the chain can never complete for it. Seeded 2026-09-07:
  07-19 (pre-VPS experiment stubs, audit refuses), 08-10..08-13 (no bybit
  recording before the 08-14 VPS phase-0 deployment), 08-22 (quarantined
  by INT-4 gate: `backpressure_loss` 6k-15k dropped frames — kept by W-6
  design). Acked days appear as cyan `[ACK ]` lines and are excluded from
  WARN/CRIT and from Telegram — the ack converts the nag into a counted
  line. New permanent refusals should get the same treatment with the
  refusal reason.

## Symptoms

Telegram dispatch `bybit-retention-check (p1|p2)` listing the offending
days, e.g. "bybit retention CRIT … never compacted: 2026-08-27 BTCUSDT;
crossed days hold 2.10 GiB". Console + `ops/scripts/bybit_retention_check.log`
lines `[CRIT]`/`[WARN]` mirror the same state; the `[TG ]` log line records
the dispatch outcome (sent / unconfigured / failed).

## Acknowledgement ledger

`data/bybit_retention_acks.jsonl` — one JSONL entry per day
(`date`/`reason`/`ts_utc`), append-only, latest entry wins. Days listed
here are exempt from CRIT: the check reports them `[ACK ]` with the reason
and counts them in the summary. Use it ONLY for days the chain genuinely
cannot complete (gate refusals on dirty data, pre-deployment gaps, junk
stubs) — never to silence a day that is simply un-run: that is a P1 by
design.

## Remediation

1. For each `never compacted` day: run the audit-gated compact
   `mp-ops compact --date <d> --venue bybit --symbol <s>`, then the prune
   `mp-ops prune --date <d> --venue bybit --symbol <s>` (refusals are W-6
   working — read the refusal reason; a quarantined day stays, and if the
   refusal is permanent, ack it: `-Ack <d> -Reason "<refusal>"`).
2. For `prune pending` days: only the prune is missing — run the printed
   command; the proof is already verified.
3. For absent-unjournaled days within the horizon: check
   `data/vps_drain_manifest.jsonl` (landed?) and the mirrors
   (`C:\mp-backup\data\raw`, off-host tier) — a deletion outside the
   journaled gate is a W-6 paper-trail gap; restore or document it (ack
   only if the absence is permanent and explained, e.g. pre-deployment).
4. Historical gap days before the absent horizon are reported as one
   counted line, never nagged — they are pre-recording or long-explained.
5. The binance-futures era (never gate-able) is a separate owner decision:
   `docs/OWNER-DECISION-binance-era.md` — do not confuse its standing
   storage-budget P2 with this check's states.

## Escalation

Persistent P1 with no refusals explains it (chain never run for the window):
the retention backlog is real and the corpus stays over-projected — run the
chain per day as the days cross. Persistent P1 WITH refusals: the days are
genuinely dirty; keep them, they show nightly until the owner decides
(accept / migrate per `docs/OWNER-DECISION-binance-era.md` is for binance;
bybit refusals are individually quarantined and keep).