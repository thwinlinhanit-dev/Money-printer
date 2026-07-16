---
name: bootstrap-workspace
description: Create the Rust cargo workspace skeleton for this repo per spec 000 (crate layout, dependency direction, CI activation, clock/lint guardrails). Use once, when the first Rust implementation task begins ("implement spec 001", "set up the workspace").
---

# Bootstrap the Cargo Workspace

Do this ONCE, as the first step of the first Rust spec task (normally spec
001). If `Cargo.toml` already exists at the repo root, this skill does not
apply — skip it.

## Layout to create (CONV-1, CONV-3)

```
Cargo.toml            [workspace] members = crates below; resolver = "2"
rust-toolchain.toml   stable, pinned minor version
.gitignore            target/, *.env, data/, raw/, cold/, runs/ (artifacts, never committed)
core/                 first real crate (spec 001)
collectors/ storage/ features/ sim/ strategies/ funnel/ oms/ risk/ ops/
                      create each crate ONLY when its spec work starts (W-4)
```

## Workspace-level settings

- `[workspace.package]`: edition, license, repository — inherited by crates.
- `[workspace.dependencies]`: pin shared deps once (serde, thiserror,
  tracing, bincode, proptest, criterion). Crates reference
  `{ workspace = true }` — one version of truth.
- `[workspace.lints.rust]` / `[workspace.lints.clippy]`:
  `unwrap_used = "warn"` (deny in decision-path crates' own lints),
  `dbg_macro = "deny"`, `todo = "warn"`.

## Dependency-direction enforcement (PD-4 mechanical)

- `deny.toml` (cargo-deny): ban `reqwest`, `tokio-tungstenite`, `hyper`,
  and any net crate from `strategies` and `features` dependency trees.
- Add `tests/dep_direction.rs` in the workspace: parse `cargo metadata`,
  assert the CONV-3 edges (strategies→features→core; sim→core; oms→core;
  no reverse edges, no strategies→oms/collectors). This is Lever 2 of
  docs/AGENT_FORCE_MULTIPLIERS.md — do not skip it "for now".

## Clock guardrail activation

`ops/ci/guardrails.sh` already greps for `SystemTime::now|Instant::now` in
Rust files outside the allowlist (`collectors`, `oms`, `ops`, tests). When
creating `core`, implement `core::Clock` (CONV-5) FIRST so there is never a
moment where decision code has no injected clock to use.

## CI activation

`.github/workflows/ci.yml` auto-detects `Cargo.toml` and starts running
fmt/clippy/test on it — no workflow edit needed. Verify the first push goes
green; a red bootstrap commit poisons every later bisect.

## Definition of done
- [ ] `cargo test` green on the skeleton (even if only smoke tests)
- [ ] dep-direction test in place and passing
- [ ] guardrails.sh passes
- [ ] `.gitignore` excludes all data/artifact dirs (W-6 protection: recorded
      data must never be commit-able by accident)
- [ ] Then proceed with the actual spec via the implement-spec skill
