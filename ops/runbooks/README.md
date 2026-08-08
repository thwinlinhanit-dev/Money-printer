# Runbooks

One file per P1/P2 alert id in the `mp-ops` registry
(`ops/src/registry.rs`), plus incident runbooks for known infrastructure
conditions that are not registry alerts (e.g. `ws-egress-filter.md`, the spec
024 Binance futures WS geo-filter). Every runbook follows the same shape:
**Symptoms**, **Diagnosis**, **Remediation** (safe, reversible steps only —
never widen a risk limit, PD-1), **Escalation**.

Adding an alert to the registry without adding its runbook here fails CI
(`ops/ci/guardrails.sh`, OPS-4). Keep these terse — they are read at 3am.
