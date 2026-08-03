# oms

## Purpose

Order management system: tracks order lifecycle, reconciliation with venue, and maintains order state machine integrity.

## Ownership

- `src/state.rs` — order state machine
- `src/reconcile.rs` — order reconciliation logic

## Verification

- `cargo test -p mp-oms`
- `cargo test -p mp-oms --test oms`

## Child DOX Index

None.
