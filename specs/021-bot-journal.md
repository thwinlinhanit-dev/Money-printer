# 021 — Bot Command Journal Persistence

## Purpose
Persist the Telegram bot command journal (currently in-memory `Vec<String>`) to append-only files so commands survive crashes, are auditable, and pending confirmations can be recovered on restart.

## Scope
In: command journal (JSONL), daily rotation, fsync-before-reply, pending confirmation persistence, audit reading. Out: Telegram bot implementation details, command handlers, authentication.

## Design

### Command journal
Append-only JSONL per day:
```
journal/telegram/commands-YYYY-MM-DD.jsonl
```

Each line:
```json
{
  "ts_ns": 1784456653319497000,
  "user_id": 123456789,
  "text": "/position BTCUSDT",
  "verdict": "ok"
}
```

### Fsync-before-reply
- Before every bot reply is sent, the command line + verdict must be fsynced to disk.
- Guarantees: if the bot acknowledges a command, the record is durable.
- Use `sync_data()` (fast, metadata not needed for append-only).

### Daily rotation
- At UTC midnight (or when file exceeds 10MB), close current file and open new one.
- Old files are read-only for audit; only current day's file is appended to.

### Pending confirmations
- Stored in `journal/telegram/pending.json` as a single JSON object.
- On every state change (confirmation created, confirmed, rejected), rewrite the file atomically (write to temp, rename).
- On restart, read `pending.json` to recover pending confirmations.

### Audit reading
- Read any date range: `journal/telegram/commands-YYYY-MM-DD.jsonl`.
- Parsable by any JSONL reader.
- Guardrail: all journal files must be `0600` on Linux (owner-only read/write), equivalent restrictive ACL on Windows.

### Concurrency
- The Telegram command handler is single-threaded (one command processed at a time). No concurrent writes to the journal.
- The `pending.json` atomic write (write to temp, rename) protects against partial writes on crash, using the same directory as the target for the rename (same filesystem guarantee).

## Requirements
- **OPS-11** Command journal MUST be persisted to `journal/telegram/commands-YYYY-MM-DD.jsonl`.
- **OPS-12** Journal MUST be fsynced before every reply is sent (durability before acknowledgment).
- **OPS-13** Journal MUST be rotated daily (or at 10MB) to avoid unbounded file growth.
- **OPS-14** Journal MUST be readable for audit: `journal/telegram/` directory, one file per day.
- **OPS-15** Pending confirmations MUST survive restart: persist to `journal/telegram/pending.json`, rewrite on state change, read on startup.

## Acceptance criteria
- [ ] Journal writes to disk
- [ ] Test: `ops_11_command_persisted` — verify JSONL output
- [ ] Test: `ops_12_fsync_before_reply` — mock slow disk, verify ordering
- [ ] Test: `ops_13_daily_rotation` — verify new file at midnight
- [ ] Test: `ops_14_audit_read` — read back 7 days, verify completeness
- [ ] Test: `ops_15_pending_survives_restart` — crash mid-confirm, verify state recovery
- [ ] Guardrail: journal files must be 0600 (`keyfile-perms` check)

## Decisions
- 2026-07-19: Format: JSONL (same as LLM archive records).
- 2026-07-19: Rotation: at UTC midnight, or when file exceeds 10MB.
- 2026-07-19: Pending: single JSON file, rewritten on every state change (small, rare).

## Open questions
- None.
