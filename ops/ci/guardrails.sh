#!/usr/bin/env bash
# Guardrails: mechanical enforcement of the CLAUDE.md rulebook.
# Runs in CI on every push and locally via the self-review skill.
# Exit nonzero on any violation. Keep checks fast, specific, low-false-positive;
# every check names the rule it enforces.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

fail=0
err() { echo "GUARDRAIL FAIL: $*" >&2; fail=1; }

tracked() { git ls-files "$@"; }

# ---- PD-2: no secrets, no real .env files -----------------------------------
if tracked | grep -qE '(^|/)\.env$|(^|/)\.env\.[^e]'; then
  # .env.example is allowed; .env and .env.production etc. are not
  tracked | grep -E '(^|/)\.env$|(^|/)\.env\.[^e]' | grep -v '\.example' | while read -r f; do
    err "PD-2: committed env file: $f"
  done
fi

if tracked -z | xargs -0 grep -lE -- '-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----' 2>/dev/null | head -1 | grep -q .; then
  err "PD-2: private key material committed"
fi

# Assigned, non-placeholder credentials in config-like files.
cred_hits=$(tracked '*.toml' '*.yaml' '*.yml' '*.json' '*.env.example' 2>/dev/null \
  | xargs -r grep -nEi '(api[_-]?key|api[_-]?secret|secret[_-]?key|access[_-]?token)\s*[:=]\s*"?[A-Za-z0-9+/_-]{16,}' 2>/dev/null \
  | grep -vEi 'example|placeholder|your[_-]|xxx|changeme|<[^>]+>' || true)
if [ -n "$cred_hits" ]; then
  echo "$cred_hits" >&2
  err "PD-2: credential-looking value in tracked config (use *.example with placeholders)"
fi

# ---- PD-1: live mode must never be committed --------------------------------
live_hits=$(tracked '*.toml' '*.yaml' '*.yml' 2>/dev/null \
  | xargs -r grep -nE '^\s*mode\s*=\s*"?live"?\s*(#.*)?$' 2>/dev/null || true)
if [ -n "$live_hits" ]; then
  echo "$live_hits" >&2
  err "PD-1: mode = live found in tracked config"
fi

# ---- PD-3 / CONV-5: no wall clock on decision paths -------------------------
# Allowlist: collectors (recv_ts stamping), oms (real order timestamps),
# ops (telemetry), tests, benches, and the ONE sanctioned wall-clock reader
# core/src/wall_clock.rs (it is what gets injected so nothing else reads time).
# Match actual calls (`::now(`) so doc-comment mentions don't false-positive.
clock_hits=$(tracked 'core/**/*.rs' 'features/**/*.rs' 'strategies/**/*.rs' \
                     'sim/**/*.rs' 'risk/**/*.rs' 'funnel/**/*.rs' 'storage/**/*.rs' 2>/dev/null \
  | grep -vE '(^|/)(tests|benches)/' \
  | grep -v 'core/src/wall_clock.rs' \
  | xargs -r grep -nE '(SystemTime|Instant|Utc|Local)::now\(' 2>/dev/null || true)
if [ -n "$clock_hits" ]; then
  echo "$clock_hits" >&2
  err "PD-3/CONV-5: wall-clock call on a decision-path crate (inject core::Clock)"
fi

# ---- PD-4 / CONV-3: strategies and features stay offline ---------------------
for crate in strategies features; do
  if [ -f "$crate/Cargo.toml" ]; then
    net_deps=$(grep -nE '^\s*(reqwest|hyper|tokio-tungstenite|tungstenite|ureq|surf|awc|isahc)\b' "$crate/Cargo.toml" || true)
    if [ -n "$net_deps" ]; then
      echo "$net_deps" >&2
      err "PD-4/CONV-3: network dependency in $crate/Cargo.toml"
    fi
  fi
done

# ---- W-7: spec index consistency ---------------------------------------------
# Every specs/NNN-*.md appears in the README status table, and vice versa.
if [ -f specs/README.md ]; then
  for f in specs/[0-9][0-9][0-9]-*.md; do
    [ -e "$f" ] || continue
    base=$(basename "$f")
    grep -q "$base" specs/README.md || err "W-7: $base missing from specs/README.md status table"
  done
  grep -oE '\]\([0-9]{3}-[a-z-]+\.md\)' specs/README.md | tr -d ']()' | while read -r ref; do
    [ -f "specs/$ref" ] || err "W-7: specs/README.md references missing spec $ref"
  done
fi

# ---- CONV-21/W-2: implemented specs must have ID-bearing tests ---------------
# For each spec marked 'implemented' in the status table, every requirement ID
# defined in it must appear (lowercased, underscored) in at least one test name.
if [ -f specs/README.md ]; then
  impl_specs=$(grep -E '^\|\s*[0-9]{3}\s*\|' specs/README.md | grep -i 'implemented' \
    | grep -oE '\([0-9]{3}-[a-z-]+\.md\)' | tr -d '()' || true)
  for spec in $impl_specs; do
    ids=$(grep -oE '\*\*[A-Z]{3,4}-[0-9]+\*\*' "specs/$spec" | tr -d '*' | sort -u)
    for id in $ids; do
      needle=$(echo "$id" | tr 'A-Z-' 'a-z_')
      # Python arm accepts pytest's mandatory test_ prefix (def test_res_5_…);
      # the bare `def res_5` form could never match a collectible pytest test.
      if ! tracked '*.rs' '*.py' 2>/dev/null | xargs -r grep -lE "fn ${needle}[a-z0-9_]*|def (test_)?${needle}[a-z0-9_]*" >/dev/null 2>&1; then
        err "CONV-21: $spec is 'implemented' but no test name embeds ${id} (expected fn/def ${needle}_*)"
      fi
    done
  done
fi

# ---- OPS-4: every registered alert has a runbook -----------------------------
# Each alert!("id", SEV) row in the ops registry MUST have ops/runbooks/id.md.
# Adding an alert without its runbook fails CI (spec 009 OPS-4).
if [ -f ops/src/registry.rs ]; then
  # Process substitution (not a pipe) so err's fail=1 reaches this shell.
  while read -r id; do
    [ -n "$id" ] || continue
    [ -f "ops/runbooks/${id}.md" ] || err "OPS-4: alert '${id}' has no ops/runbooks/${id}.md"
  done < <(grep -oE 'alert!\("[a-z0-9-]+"' ops/src/registry.rs | sed -E 's/alert!\("([a-z0-9-]+)"/\1/')
fi

# ---- CONV-3: one crate per top-level dir (workspace members are real dirs) ----
if [ -f Cargo.toml ]; then
  while read -r member; do
    [ -n "$member" ] || continue
    [ -f "$member/Cargo.toml" ] || err "CONV-3: workspace member '$member' has no $member/Cargo.toml"
    case "$member" in
      */*) err "CONV-3: workspace member '$member' is not a top-level directory" ;;
    esac
  done < <(grep -E '^\s*members\s*=' Cargo.toml | grep -oE '"[a-z0-9_-]+"' | tr -d '"')
fi

# ---- Skill frontmatter sanity -------------------------------------------------
for f in .claude/skills/*/SKILL.md; do
  [ -e "$f" ] || continue
  head -1 "$f" | grep -q '^---$' || err "skill $f missing YAML frontmatter"
  grep -q '^name:' "$f" && grep -q '^description:' "$f" || err "skill $f missing name/description"
done

if [ "$fail" -ne 0 ]; then
  echo "" >&2
  echo "Guardrails failed. Rules live in CLAUDE.md; do not weaken this script to pass (PD-5)." >&2
  exit 1
fi
echo "guardrails: all checks passed"
