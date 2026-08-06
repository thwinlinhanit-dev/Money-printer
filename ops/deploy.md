# Deployment (OPS-8)

Reproducible bring-up of the recorder + ops plane on a fresh VPS. Follow this
verbatim; a fresh host should reach *running-collector* state with no other
knowledge. **Last verified: not yet run on a clean host — record the date here
the first time you do (OPS-8).**

> PD-1/PD-2: this doc never enables live trading and never contains secrets.
> Keys live in `*.env` files created on the host (mode 0600), never in the repo.

## 0. Host

- Non-US region (Binance blocks US IPs — COL geo note).
- Docker + Compose *or* systemd. Both layouts are provided; pick one.
- A dedicated service user (`printer`), never root, for OPS-7 file perms.

## 1. Secrets (on the host only)

```sh
install -m 0600 /dev/null /etc/money-printer/venues.env   # public data: usually empty
install -m 0600 /dev/null /etc/money-printer/ops.env      # TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID, OWNER_ID
install -m 0600 /dev/null /etc/money-printer/llm.env      # ANTHROPIC_API_KEY, ... (see llm/providers.example.toml)
```

Fill them with a host-side editor. `chmod 0600` is enforced by the
`keyfile-perms` alert (OPS-7).

## 2. Build

```sh
cargo build --release --features live-ws        # collectors with the WS transport
cargo build --release -p mp-ops
cargo build --release -p mp-features --bin whale_study   # RES-4 study binary
```

Install the built binaries into `/opt/money-printer/bin/` (`mp-collector`,
`opsd`, `whale_study`) and the weekly-study wrapper (exec bit kept); the
research job scripts live under `/opt/money-printer/research/` (as the
grading/brief units already assume):

```sh
install -d /opt/money-printer/ops/scripts
install -m 0755 ops/scripts/run_whale_study_weekly.sh /opt/money-printer/ops/scripts/
install -m 0644 research/run_band_accuracy.py research/band_accuracy.py research/band_accuracy_job.py /opt/money-printer/research/
```

## 3. Run (systemd)

Copy the unit templates from `ops/systemd/` (below), fill the venue list, then:

```sh
systemctl enable --now collector@bybit collector@okx collector@binance
systemctl enable --now opsd
systemctl enable --now whale-study.timer
systemctl enable --now telegram-stale.timer
systemctl status 'collector@*' opsd whale-study.timer telegram-stale.timer
```

`whale-study.timer` runs the RES-4 band-accuracy study weekly (Tue 06:30 UTC)
and is `ConditionPathExists`-gated: it skips (never fails) until the spec 028
`mp-whale` collector has written `data/raw/*_hyperliquid_positions.log`, then
journals each run's SIM-10 record to `runs/index.jsonl`. After each study the
wrapper also runs `mp-ops band-accuracy-decay --runs-dir
/opt/money-printer/runs --telegram` over the trend journal (OPS-13 P3
drift/decay watch) — the weekly verdict is journaled to the same
`runs/index.jsonl` as a `band_accuracy_decay` record line correlated to the
study's run by `run_id`/`week`, and `mp-ops` installs to
`/opt/money-printer/bin/` above. When the decay P3 fires it is delivered to
Telegram (quiet-hours batched, drained just after 07:00 UTC via
`mp-ops telegram-flush --wait`): the host needs
`curl`, and ops.env must carry `TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID`
(the bot token spec 021 uses). `telegram-stale.timer` runs
`mp-ops telegram-stale --telegram` hourly (OPS-14): a dispatch queued
longer than one quiet window (24h — a missed flush) raises the
`telegram-stale` P2 immediately — P2 breaks through quiet hours, so it is
sent right away, never re-queued into the stuck batch. Make sure the
`mp-whale` census collector (spec 028) is running, or the study stays a skip.

Confirm `/status` in Telegram shows every collector heartbeating.

## 4. Run (Docker Compose)

```sh
docker compose -f ops/compose.yaml up -d
docker compose -f ops/compose.yaml ps
```

## 5. Dead-man wiring (OPS-2)

- Each process POSTs `/beat/{proc}` to `opsd` every 30s.
- `opsd` pings an EXTERNAL healthcheck (healthchecks.io-style) every 5 min so
  the watcher has a watcher. Set `OPSD_EXTERNAL_PING_URL` in `ops.env`.

## 6. Verify

- Chaos test: `systemctl kill -s SIGKILL collector@bybit` ⇒ restarts, dead-man
  does NOT fire (within the 90s window). Stop it fully ⇒ alert within 2 min.
- `/kill GLOBAL` writes the kill-latch file the gate reads (RG-10) — test it
  reaches the gate even with oms stopped.

## 7. Backups (OPS-5)

Nightly encrypted tarball of `journal/`, `runs/index.sqlite`, configs, funnel
state → off-host (rclone). Quarterly: run `ops/restore-drill.sh` — an untested
backup is a hope, not a backup.
