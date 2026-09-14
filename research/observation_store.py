"""Python reader for the Rust observation Parquet store (spec 054 REL-34).

ONE artifact store, TWO consumers: the Rust evaluation engine
(`mp-storage::observation_store`) writes identity-stamped observations;
this module is the Python grading arm's reader for the same files — never
a parallel export, never a re-write.

Contract enforced on every load (a store the grader cannot cryptographically
place must not be graded):

1. Schema: the Arrow schema (names + types, in order) must equal
   ``OBSERVATION_SCHEMA`` — any divergence is a hard error.
2. Identity fingerprint re-verification, cross-language: Python re-implements
   the Rust FNV-1a fingerprint over (signal_id ‖ feature_version LE ‖
   data_schema_version LE ‖ params_hash LE ‖ cost_model_hash LE) and must
   reproduce the on-disk identity directory name for every file loaded.
3. Single identity: a load spanning files from >1 fingerprint raises
   (R-3: grades are per-identity or nothing).

On-disk layout (written by ``partitioned_write``): ONE ROW PER
(observation, outcome horizon). Outcome-less observations store a single row
with all ``outcome_*`` columns NULL. Horizon selection therefore happens by
``outcome_horizon_ns`` with ``observation_id`` as the row-identity key.

Pure readers: no wall clock, no network (PD-3). Deterministic ordering.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

# --- Column schema (must mirror storage/src/observation_store.rs) ----------

# Arrow DataType objects (not strings): str(pa.float64()) is "double", so
# object equality is the only robust contract.
COLUMNS: tuple[tuple[str, "pa.DataType"], ...] = (
    ("observation_id", pa.uint64()),
    ("signal_id", pa.string()),
    ("feature_version", pa.uint16()),
    ("data_schema_version", pa.uint16()),
    ("params_hash", pa.string()),
    ("cost_model_hash", pa.string()),
    ("ts_ns", pa.int64()),
    ("symbol_id", pa.uint32()),
    ("venue_code", pa.uint16()),
    ("direction", pa.int8()),
    ("quality", pa.uint8()),
    ("created_at_ns", pa.int64()),
    ("snapshot_json", pa.string()),
    ("outcome_horizon_ns", pa.int64()),
    ("outcome_entry_price", pa.float64()),
    ("outcome_exit_price", pa.float64()),
    ("outcome_gross_return", pa.float64()),
    ("outcome_net_return", pa.float64()),
    ("outcome_mfe", pa.float64()),
    ("outcome_mae", pa.float64()),
    ("outcome_hit", pa.uint8()),
)

OUTCOME_COLUMNS: tuple[str, ...] = (
    "outcome_horizon_ns",
    "outcome_entry_price",
    "outcome_exit_price",
    "outcome_gross_return",
    "outcome_net_return",
    "outcome_mfe",
    "outcome_mae",
    "outcome_hit",
)

#: DataQualityState storage codes (features/src/data_quality.rs declaration
#: order 0..5; identity/state semantics per R-1 — code != 0 blocks firing).
QUALITY_CODES: dict[str, int] = {
    "Healthy": 0,
    "InsufficientHistory": 1,
    "Stale": 2,
    "Invalid": 3,
    "Missing": 4,
    "Gap": 5,
}
QUALITY_NAMES: dict[int, str] = {v: k for k, v in QUALITY_CODES.items()}

DIRECTION_LONG = 1
DIRECTION_SHORT = -1

OBSERVATIONS_DIRNAME = "observations"

# --- FNV-1a (must mirror core/src/hash.rs + signal_identity.rs) -------------

_FNV_OFFSET = 0xCBF29CE484222325
_FNV_PRIME = 0x100000001B3
_U64_MASK = (1 << 64) - 1


def _fnv1a_absorb(h: int, data: bytes) -> int:
    for b in data:
        h = ((h ^ b) * _FNV_PRIME) & _U64_MASK
    return h


def identity_fingerprint(
    signal_id: str,
    feature_version: int,
    data_schema_version: int,
    params_hash: str,
    cost_model_hash: str,
) -> str:
    """Cross-language mirror of ``SignalResearchIdentity::fingerprint``.

    FNV-1a over signal_id bytes, feature_version LE, data_schema_version LE,
    params_hash bytes, cost_model_hash bytes — formatted ``{h:016x}``.
    """
    h = _fnv1a_absorb(_FNV_OFFSET, signal_id.encode("utf-8"))
    h = _fnv1a_absorb(h, feature_version.to_bytes(2, "little"))
    h = _fnv1a_absorb(h, data_schema_version.to_bytes(2, "little"))
    h = _fnv1a_absorb(h, params_hash.encode("utf-8"))
    h = _fnv1a_absorb(h, cost_model_hash.encode("utf-8"))
    return f"{h:016x}"


class ObservationStoreError(RuntimeError):
    """The store on disk cannot be graded as-is (schema/fingerprint/identity)."""


# --- Loaded rows -------------------------------------------------------------


@dataclass(frozen=True)
class ObservationRow:
    """One (observation, outcome horizon) row of the store."""

    observation_id: int
    signal_id: str
    feature_version: int
    data_schema_version: int
    params_hash: str
    cost_model_hash: str
    identity_fingerprint: str
    ts_ns: int
    symbol_id: int
    venue_code: int
    direction: int
    quality_code: int
    created_at_ns: int
    snapshot_json: str
    outcome_horizon_ns: int | None
    outcome_entry_price: float | None
    outcome_exit_price: float | None
    outcome_gross_return: float | None
    outcome_net_return: float | None
    outcome_mfe: float | None
    outcome_mae: float | None
    outcome_hit: int | None


@dataclass(frozen=True)
class ObservationStore:
    """A verified single-identity view of one identity directory."""

    root: Path
    fingerprint: str
    rows: tuple[ObservationRow, ...]

    @property
    def signal_id(self) -> str:
        return self.rows[0].signal_id

    @property
    def identity(self) -> dict[str, str | int]:
        """The full identity stamp shared by every row (REL-3/R-3)."""
        r = self.rows[0]
        return {
            "signal_id": r.signal_id,
            "feature_version": r.feature_version,
            "data_schema_version": r.data_schema_version,
            "params_hash": r.params_hash,
            "cost_model_hash": r.cost_model_hash,
            "identity_fingerprint": r.identity_fingerprint,
        }

    def net_outcomes(self, horizon_ns: int) -> list[ObservationRow]:
        """Rows with a CLOSED net outcome at ``horizon_ns`` — one per
        observation, deterministic order. NULL-outcome rows (open windows)
        are absent by construction: honest denominator, never imputed (R-1).
        """
        seen: set[int] = set()
        out: list[ObservationRow] = []
        for r in self.rows:
            if (
                r.outcome_horizon_ns == horizon_ns
                and r.outcome_net_return is not None
                and r.observation_id not in seen
            ):
                seen.add(r.observation_id)
                out.append(r)
        return out


# --- Loading -----------------------------------------------------------------


def _parquet_files(identity_dir: Path) -> list[Path]:
    files = sorted(
        p
        for p in identity_dir.iterdir()
        if p.is_file() and p.suffix == ".parquet" and p.name.startswith("date=")
    )
    if not files:
        raise ObservationStoreError(
            f"no observation parquet files under {identity_dir}"
        )
    return files


def _verify_schema(table_path: Path, table: "pa.Table") -> None:
    want = list(COLUMNS)
    got = [(f.name, f.type) for f in table.schema]
    if got != want:
        want_s = [(n, str(t)) for n, t in want]
        got_s = [(n, str(t)) for n, t in got]
        raise ObservationStoreError(
            f"schema drift in {table_path.name}: store reader expects {want_s}, got {got_s}"
        )


def _verify_fingerprint(table_path: Path, cols: dict[str, list], fp: str) -> None:
    """Re-derive the fingerprint from EVERY row's identity columns — a file
    whose rows disagree with each other or with the directory name is
    unplaceable and refused (R-3: per-identity or nothing)."""
    seen: set[tuple[str, int, int, str, str]] = set()
    n = len(cols["signal_id"])
    for i in range(n):
        seen.add(
            (
                cols["signal_id"][i],
                int(cols["feature_version"][i]),
                int(cols["data_schema_version"][i]),
                cols["params_hash"][i],
                cols["cost_model_hash"][i],
            )
        )
    if len(seen) != 1:
        raise ObservationStoreError(
            f"identity-mixing rows in {table_path.name}: {sorted(seen)} "
            "(R-3: every row of a file must carry one identity)"
        )
    signal_id, fv, dv, ph, ch = next(iter(seen))
    expect = identity_fingerprint(
        signal_id=signal_id,
        feature_version=fv,
        data_schema_version=dv,
        params_hash=ph,
        cost_model_hash=ch,
    )
    if expect != fp:
        raise ObservationStoreError(
            f"identity fingerprint mismatch in {table_path.name}: directory says {fp}, "
            f"columns re-derive {expect} (R-3: unplaceable store — refusing to grade)"
        )


def _load_identity_dir(identity_dir: Path) -> ObservationStore:
    fp = identity_dir.name
    rows: list[ObservationRow] = []
    for path in _parquet_files(identity_dir):
        table = pq.read_table(path)
        _verify_schema(path, table)
        if table.num_rows == 0:
            continue
        cols = {name: table.column(name).to_pylist() for name, _ in COLUMNS}
        _verify_fingerprint(path, cols, fp)
        for i in range(table.num_rows):
            rows.append(
                ObservationRow(
                    observation_id=cols["observation_id"][i],
                    signal_id=cols["signal_id"][i],
                    feature_version=cols["feature_version"][i],
                    data_schema_version=cols["data_schema_version"][i],
                    params_hash=cols["params_hash"][i],
                    cost_model_hash=cols["cost_model_hash"][i],
                    identity_fingerprint=fp,
                    ts_ns=cols["ts_ns"][i],
                    symbol_id=cols["symbol_id"][i],
                    venue_code=cols["venue_code"][i],
                    direction=cols["direction"][i],
                    quality_code=cols["quality"][i],
                    created_at_ns=cols["created_at_ns"][i],
                    snapshot_json=cols["snapshot_json"][i],
                    outcome_horizon_ns=cols["outcome_horizon_ns"][i],
                    outcome_entry_price=cols["outcome_entry_price"][i],
                    outcome_exit_price=cols["outcome_exit_price"][i],
                    outcome_gross_return=cols["outcome_gross_return"][i],
                    outcome_net_return=cols["outcome_net_return"][i],
                    outcome_mfe=cols["outcome_mfe"][i],
                    outcome_mae=cols["outcome_mae"][i],
                    outcome_hit=cols["outcome_hit"][i],
                )
            )
    if not rows:
        raise ObservationStoreError(f"identity dir {identity_dir} contains no rows")
    bad = [r.quality_code for r in rows if r.quality_code not in QUALITY_NAMES]
    if bad:
        raise ObservationStoreError(
            f"unknown quality code {bad[0]} in {identity_dir} (vocabulary drift)"
        )
    mixed = {r.identity_fingerprint for r in rows}
    if len(mixed) != 1:
        raise ObservationStoreError(
            f"identity-mixing load refused: {sorted(mixed)} (R-3: per-identity or nothing)"
        )
    return ObservationStore(root=identity_dir, fingerprint=fp, rows=tuple(rows))


def load_observation_store(obs_dir: Path | str, fingerprint: str) -> ObservationStore:
    """Load ONE identity directory, fully verified. Deterministic row order
    (files sorted by name, rows in file order)."""
    identity_dir = Path(obs_dir) / OBSERVATIONS_DIRNAME / fingerprint
    if not identity_dir.is_dir():
        raise ObservationStoreError(f"identity directory not found: {identity_dir}")
    return _load_identity_dir(identity_dir)


def discover_identities(obs_dir: Path | str) -> list[str]:
    """All identity fingerprints present under ``obs_dir``, sorted."""
    base = Path(obs_dir) / OBSERVATIONS_DIRNAME
    if not base.is_dir():
        return []
    return sorted(d.name for d in base.iterdir() if d.is_dir())
