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
install -m 0600 /dev/null /etc/money-printer/ops.env      # TELEGRAM_BOT_TOKEN, OWNER_ID
install -m 0600 /dev/null /etc/money-printer/llm.env      # ANTHROPIC_API_KEY, ... (see llm/providers.example.toml)
```

Fill them with a host-side editor. `chmod 0600` is enforced by the
`keyfile-perms` alert (OPS-7).

## 2. Build

```sh
cargo build --release --features live-ws        # collectors with the WS transport
cargo build --release -p mp-ops
```

## 3. Run (systemd)

Copy the unit templates from `ops/systemd/` (below), fill the venue list, then:

```sh
systemctl enable --now collector@bybit collector@okx collector@binance
systemctl enable --now opsd
systemctl status 'collector@*' opsd
```

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
