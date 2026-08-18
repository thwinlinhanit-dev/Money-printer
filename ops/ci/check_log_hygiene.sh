#!/usr/bin/env bash
# Log hygiene (LOG-1): trace/watchdog logs are machine input — the gate, the
# audit, and the post-mortem tooling parse timestamps out of them, so they
# must stay plain text. 2026-08-15: the collector's --trace-file sink emitted
# tracing-subscriber's default ANSI color codes (ESC[2m … ESC[0m) at the head
# of every line, which broke timestamp extraction during the outage
# investigation; fixed with .with_ansi(false) in
# collectors/src/binutil.rs::trace_subscriber.
#
# Checks:
#   1. Source contract: the trace sink keeps ANSI disabled (grep the shared
#      builder — a regression here silently re-poisons every future trace).
#   2. Format contract on committed fixtures (ops/ci/fixtures/*_hygiene.txt):
#      no 0x1b escape bytes, no UTF-8 BOM, every non-empty line starts with a
#      parseable UTC timestamp (trace: `2026-08-15T00:36:23...Z`;
#      watchdog: `[2026-08-15T00:35:42Z]`).
#   3. The same script accepts real log paths for host-side runs:
#        bash ops/ci/check_log_hygiene.sh data/raw/trace_*.log
#      (Note: pre-2026-08-18 watchdog logs carry a PS 5.1 UTF-8 BOM from
#      Add-Content -Encoding UTF8 — pre-existing pipeline behavior the Rust
#      gate reads fine; new logs should be BOM-free.)
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

fail=0
err() { echo "LOG-HYGIENE FAIL: $*" >&2; fail=1; }

# ---- 1. source contract -------------------------------------------------------
if [ -f collectors/src/binutil.rs ]; then
  if ! grep -q 'with_ansi(false)' collectors/src/binutil.rs; then
    err "collectors/src/binutil.rs: trace sink lost .with_ansi(false) (LOG-1; 2026-08-15 ANSI pollution)"
  fi
fi

# ---- 2/3. per-file checks -----------------------------------------------------
files=("$@")
if [ "${#files[@]}" -eq 0 ]; then
  files=(ops/ci/fixtures/*_hygiene.txt)
fi

ts_re='^\[?[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}'
for f in "${files[@]}"; do
  [ -f "$f" ] || { err "$f: missing"; continue; }

  # no ANSI escape bytes (0x1b)
  if LC_ALL=C grep -q $'\x1b' "$f"; then
    err "$f: contains ANSI escape bytes (0x1b)"
  fi

  # no UTF-8 BOM at byte 0
  if head -c 3 "$f" | od -An -tx1 | tr -d ' \n' | grep -q '^efbbbf$'; then
    err "$f: starts with a UTF-8 BOM"
  fi

  # every non-empty line starts with a parseable UTC timestamp (one grep
  # pass — per-line subprocess spawning is too slow on multi-MB traces)
  if bad=$(LC_ALL=C grep -vnE "$ts_re" "$f" | grep -v ':[[:space:]]*$' | head -3); then
    err "$f: lines lack timestamp prefix: $bad"
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "" >&2
  echo "Log hygiene failed. Trace/watchdog logs must stay machine-parseable." >&2
  exit 1
fi
echo "log-hygiene: all checks passed"
