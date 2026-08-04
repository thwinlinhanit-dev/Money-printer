#!/usr/bin/env bash
# Requirement-coverage report (backlog: agent infrastructure).
#
# Companion to guardrails.sh's CONV-21 check: instead of just failing on a gap,
# print a human-readable matrix of every spec -> requirement ID -> matching test
# (Rust `fn <id>_*`, Python `def test_<id>_*` / `def <id>_*`). Intended as a
# CI artifact and a session-start health readout so an agent begins knowing the
# tree's requirement->test coverage.
#
# Exit: 0 = every requirement ID across every spec has an ID-bearing test;
#       1 = at least one gap (same rule guardrails.sh enforces for
#           'implemented' specs, here applied to all specs for visibility).
set -uo pipefail
cd "$(git rev-parse --show-toplevel)"

fail=0
for spec in specs/[0-9][0-9][0-9]-*.md; do
  [ -e "$spec" ] || continue
  base=$(basename "$spec")
  printf '\n== %s ==\n' "$base"
  ids=$(grep -oE '\*\*[A-Z]{3,4}-[0-9]+\*\*' "$spec" | tr -d '*' | sort -u)
  if [ -z "$ids" ]; then
    printf '   (no **PREFIX-N** requirement IDs)\n'
    continue
  fi
  for id in $ids; do
    needle=$(echo "$id" | tr 'A-Z-' 'a-z_')
    if git ls-files '*.rs' '*.py' 2>/dev/null \
        | xargs -r grep -lE "fn ${needle}[a-z0-9_]*|def (test_)?${needle}[a-z0-9_]*" >/dev/null 2>&1; then
      printf '   [ OK ] %s\n' "$id"
    else
      printf '   [GAP ] %s  ->  missing test (want fn/def %s_*)\n' "$id" "$needle"
      fail=1
    fi
  done
done

printf '\n'
if [ "$fail" -eq 0 ]; then
  echo "coverage: every requirement ID has an ID-bearing test"
else
  echo "coverage: ${fail} gap(s) found (informational — see [GAP] rows above)" >&2
  echo "NOTE: gaps in draft/implementing specs are expected; guardrails.sh" >&2
  echo "      remains the gate and only fails on 'implemented' spec gaps." >&2
fi
# This is a report/artifact, not a gate: always exit 0 so CI publishes the
# matrix. The binding check lives in guardrails.sh (CONV-21, implemented specs).
exit 0
