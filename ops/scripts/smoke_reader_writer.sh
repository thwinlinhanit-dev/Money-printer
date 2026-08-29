#!/usr/bin/env bash
# smoke_reader_writer.sh — post-deploy reader/writer pair smoke (OPS).
#
# Incident 2026-08-22 follow-up: a deploy that rebuilds SOME binaries can
# ship a reader/writer split (the Aug-18 deploy's mp-ops rejected schema-4
# logs its own collectors wrote — five nightly scorecards read all-zero and
# the promotion gate went blind). Run this IMMEDIATELY after any partial
# binary deploy, before trusting the next night's gate.
#
# What it proves: the INSTALLED mp-ops decodes what the RUNNING collectors
# are writing, today, on this host. It scores today's partial day-files and
# requires every required recording to carry event_count > 0 — an all-zero
# or refused scorecard fails the smoke (mp-ops itself refuses to emit a
# verdict from zero bytes since the 2026-08-25 plausibility guard).
#
# Usage:  bash smoke_reader_writer.sh [YYYYMMDD] [venue:SYMBOL ...]
#   date defaults to today UTC; recordings default to ops/core_symbols.txt.
# Env:    MP_OPS_CMD   (default /opt/money-printer/bin/mp-ops)
#         MP_CORE_SYMBOLS (default /opt/money-printer/ops/core_symbols.txt)
# Exit:   0 pass · 1 fail (details on stderr)
set -uo pipefail

MP_OPS_CMD="${MP_OPS_CMD:-/opt/money-printer/bin/mp-ops}"
MP_CORE_SYMBOLS="${MP_CORE_SYMBOLS:-/opt/money-printer/ops/core_symbols.txt}"
DATE="${1:-$(date -u +%Y%m%d)}"
shift || true

REQUIRED=("$@")
if [ "${#REQUIRED[@]}" -eq 0 ]; then
  if [ ! -r "$MP_CORE_SYMBOLS" ]; then
    echo "smoke: core symbols file unreadable: $MP_CORE_SYMBOLS" >&2
    exit 1
  fi
  mapfile -t REQUIRED < <(grep -v '^[[:space:]]*#' "$MP_CORE_SYMBOLS" | grep -v '^[[:space:]]*$')
fi
if [ "${#REQUIRED[@]}" -eq 0 ]; then
  echo "smoke: no required recordings resolved" >&2
  exit 1
fi
if [ ! -x "$MP_OPS_CMD" ]; then
  echo "smoke: mp-ops not executable: $MP_OPS_CMD" >&2
  exit 1
fi

ARGS=(scorecard --date "$DATE")
for req in "${REQUIRED[@]}"; do
  ARGS+=(--required "$req")
done

echo "smoke: scoring TODAY ($DATE) through the installed pair: $MP_OPS_CMD"
if ! OUT=$("$MP_OPS_CMD" "${ARGS[@]}" 2>/dev/null); then
  echo "smoke: FAIL - installed mp-ops could not produce today's verdict." >&2
  echo "smoke: Either sources are missing or it cannot decode current-schema logs" >&2
  echo "smoke: (reader/writer split). Do NOT trust tonight's gate; rebuild from" >&2
  echo "smoke: current sources and re-run this smoke." >&2
  exit 1
fi

FAIL=0
PARSED=0
while IFS= read -r line; do
  [ -z "$line" ] && continue
  PARSED=$((PARSED + 1))
  venue=$(printf '%s' "$line" | sed -n 's/.*"venue":"\([^"]*\)".*/\1/p')
  symbol=$(printf '%s' "$line" | sed -n 's/.*"symbol":"\([^"]*\)".*/\1/p')
  count=$(printf '%s' "$line" | sed -n 's/.*"event_count":\([0-9]*\).*/\1/p')
  if [ -z "$count" ] || [ "$count" -eq 0 ]; then
    echo "smoke: FAIL - $venue:$symbol decoded to 0 events from a live log" >&2
    FAIL=1
  else
    echo "smoke: ok   - $venue:$symbol event_count=$count"
  fi
done <<EOF
$(printf '%s\n' "$OUT" | grep '"event_count"')
EOF

# Vacuous-pass guard (audit 2026-08-26): if the scorecard output contains NO
# parseable recording lines at all (mp-ops output shape changed), the loop
# above would silently pass with zero assertions made. A smoke that proves
# nothing must fail like a smoke that fails.
if [ "$PARSED" -lt "${#REQUIRED[@]}" ]; then
  echo "smoke: FAIL - parsed $PARSED recording line(s) but ${#REQUIRED[@]} required; mp-ops output shape changed or sources missing" >&2
  exit 1
fi

if [ "$FAIL" -ne 0 ]; then
  echo "smoke: FAIL - reader/writer pair is broken; gate verdicts are not evidence" >&2
  exit 1
fi
echo "smoke: PASS - installed reader decodes live collector output for ${#REQUIRED[@]} recording(s)"
