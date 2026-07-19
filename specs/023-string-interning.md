# 023 — String Interning in Features

## Purpose
Eliminate heap-allocated `String` fields from hot-path feature types (`Cvd`, `FeatureUpdate`) by using interned identifiers (`SymbolId`, `Venue`). This reduces allocation pressure and makes more types `Copy`.

## Scope
In: `FeatureUpdate.feature` → `SymbolId`, `Cvd.venue_slug` → `Venue`, `WhalePrint` feature names → interned, feature registration interning at setup time, schema bump. Out: other feature structs, non-feature string usage, config parsing.

## Design

### Current allocation sites
| Struct | Field | Current type | Problem |
|--------|-------|-------------|---------|
| `Cvd` | `venue_slug` | `String` | Allocated per event; used only for `id()` and display |
| `WhalePrint` | `venue` | `Venue` (Copy) | ✅ already interened (enum variant) |
| `FeatureUpdate` | `feature` | `String` | Allocated per update; used as feature identifier |

### After interning
| Struct | Field | New type | Saved |
|--------|-------|----------|-------|
| `Cvd` | `venue` | `Venue` (enum, Copy) | 1 alloc per event, replaces `venue_slug` |
| `FeatureUpdate` | `feature` | `SymbolId` (u32, Copy) | 1 alloc per update |

### Feature registration
At setup time, `FeatureEngine` interns all feature names. `SymbolId(0)` is reserved as null/invalid:

```rust
const NULL_SYMBOL: SymbolId = SymbolId(0);

pub fn register(&mut self, name: &str) -> SymbolId {
    // starts from 1; SymbolId(0) reserved as null
    *self.name_table.entry(name.to_string()).or_insert_with(|| {
        let id = self.next_id;
        self.next_id += 1;
        id
    })
}
```

`SymbolId` is already defined in `core` — reuse it. Symbol IDs start at 1.

### Schema impact
- `FeatureUpdate.feature` changes from `String` to `SymbolId`.
- This is a schema change: bump `schema_ver` for feature updates.
- The feature name table must be persisted alongside Parquet output (spec 016) so offline replay can resolve `SymbolId → name`.

### Config impact
Feature config still uses human-readable strings:
```toml
[feature.cvd]
name = "cvd"
```
Interning happens at config parse time, not at runtime.

## Requirements
- **FEA-15** `Cvd.venue_slug: String` MUST become `Cvd.venue: Venue` (Venue is already Copy — `WhalePrint.venue` shows the pattern).
- **FEA-16** `FeatureUpdate.feature: String` MUST become `FeatureUpdate.feature: SymbolId` (interned u32).
- **FEA-17** Feature registration MUST intern names at setup time, not runtime.
- **FEA-18** `SymbolId(0)` MUST be reserved as null/invalid; registration starts from 1.

## Acceptance criteria
- [ ] No `String` in hot-path feature structs (config-only strings are fine)
- [ ] Test: `fea_15_cvd_uses_venue_instead_of_string` — Cvd.venue_slug removed, Cvd.venue: Venue
- [ ] Test: `fea_16_feature_update_uses_symbol_id` — FeatureUpdate.feature is SymbolId, not String
- [ ] Test: `fea_17_interned_names_stable` — same name → same SymbolId
- [ ] Test: `fea_18_symbol_id_zero_reserved` — verify SymbolId(0) returns None from lookup
- [ ] Test: `fea_19_no_heap_alloc_on_hot_path` — alloc probe, verify zero allocs per event for feature types
- [ ] Test: `fea_20_parquet_roundtrip_with_interned_ids` — write FeatureUpdate with SymbolId, read back, verify names resolve

## Decisions
- 2026-07-19: `SymbolId` already exists in core — reuse it for feature names.
- 2026-07-19: Feature name table lives in `FeatureEngine`, populated at registration.
- 2026-07-19: This is a breaking change to `FeatureUpdate` — bump `schema_ver`.

## Open questions
- None.
