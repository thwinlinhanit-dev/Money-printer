# telegram-stale (P2)

A dispatch has sat in the quiet-hours Telegram batch ledger
(`journal/telegram/batch.jsonl`) longer than one full quiet window (default
24h) — a `telegram-flush` was missed or failed. The monthly report only
flags a stuck queue at month-end; this alert is the near-real-time watch
(OPS-14, hourly via `telegram-stale.timer`), and P2 means it breaks through
quiet hours — it is never re-queued into the very batch that is stuck.

## Symptoms
- Alert names the stuck dispatch(es): e.g. "1 dispatch(es) queued ≥ 24h
  (missed flush); oldest 'band-accuracy-decay' queued 30h".
- `journal/telegram/batch.jsonl` still holds lines while
  `journal/telegram/delivered.jsonl` shows nothing new.

## Diagnosis
- Check the flush ran: `systemctl list-timers | grep whale-study` (the
  wrapper's flush is a best-effort hook — a missing/old `mp-ops` binary or a
  wedged host skips it silently by design, which is exactly the gap this
  alert closes).
- Inspect the ledger (absolute — the unit resolves it via
  `WorkingDirectory=/opt/money-printer`): `wc -l
  /opt/money-printer/journal/telegram/batch.jsonl` and `tail -3
  /opt/money-printer/journal/telegram/batch.jsonl` (each line's `ts_ns` is
  when it was queued). A line much older than the last quiet window is the
  stuck one.
- Try the flush by hand (idempotent, at-most-once per attempt):
  `mp-ops telegram-flush --dir /opt/money-printer/journal/telegram --wait`
  with `TELEGRAM_BOT_TOKEN`/`TELEGRAM_CHAT_ID` set; a failed send keeps the
  batch and exits 2 with the reason (e.g. `curl` missing, wrong token,
  quiet-hours env misconfigured).

## Remediation
- P2: the alerts are FYI-grade, but the batch is the delivery ledger — flush
  it now: `sudo -u printer /opt/money-printer/bin/mp-ops telegram-flush`
  (outside quiet hours, or with `--wait` during them). The delivery log
  records what actually went out (W-6).
- If `telegram-flush` itself fails: check `curl` is installed, the Bot API
  token is live (the detail's failure mode is surfaced on stderr), and that
  the ledger line parses as a `Dispatch` (a corrupt line aborts the flush
  deliberately, CONV-8 — fix it by hand, never delete it).
- Do NOT delete or rewrite the batch ledger to silence the alert (W-6): a
  stuck line is evidence and must be delivered or explicitly accounted for.

## Escalation
A flush that stays broken across a full day (the alert re-fires each hour)
means the Telegram edge itself is down — treat as a P2 outage of the alert
channel: check `opsd`/host health and the `ops.env` credentials before the
channel loss compounds with whatever the stuck alerts were carrying.
