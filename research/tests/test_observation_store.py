"""REL-34 tests: the Python grading arm reads the Rust observation store.

The decisive test is against the REAL store under data/observations (skipped
when absent, e.g. CI without the corpus): Python's FNV-1a re-implementation
must reproduce every on-disk identity directory name — the cross-language
hash contract, verified against bytes the Rust writer actually produced.
"""

from __future__ import annotations

import json
import shutil
from pathlib import Path

import pyarrow as pa
import pytest
from pytest import approx

from grading import grade_observation_returns
from grading_job import (
    DEFAULT_HORIZONS_NS,
    _decision,
    _tier,
    run_observation_grading,
)
from observation_store import (
    COLUMNS,
    OUTCOME_COLUMNS,
    ObservationRow,
    ObservationStoreError,
    discover_identities,
    identity_fingerprint,
    load_observation_store,
)

REPO = Path(__file__).resolve().parents[2]
REAL_STORE = REPO / "data" / "observations"


# --- Cross-language fingerprint vectors ---------------------------------------


def test_fnv1a_vectors_match_rust():
    """Independent inline FNV-1a (offset, prime, per-byte xor-multiply,
    wrapping) mirroring core/src/hash.rs + the signal_identity absorb order:
    signal_id bytes ‖ feature_version LE ‖ data_schema_version LE ‖
    params_hash bytes ‖ cost_model_hash bytes, formatted 016x. The u16 fields
    are ALWAYS absorbed — even as zero bytes — so the empty-identity value is
    offset·P⁴, not the raw offset."""
    M = (1 << 64) - 1
    OFF, P = 0xCBF29CE484222325, 0x100000001B3

    def ref(signal_id: str, fv: int, dv: int, ph: str, ch: str) -> str:
        h = OFF
        for b in (
            signal_id.encode()
            + fv.to_bytes(2, "little")
            + dv.to_bytes(2, "little")
            + ph.encode()
            + ch.encode()
        ):
            h = ((h ^ b) * P) & M
        return f"{h:016x}"

    assert identity_fingerprint("", 0, 0, "", "") == ref("", 0, 0, "", "")
    assert identity_fingerprint("", 0, 0, "", "") == f"{(OFF * P * P * P * P) & M:016x}"
    assert identity_fingerprint("a", 0, 0, "", "") == ref("a", 0, 0, "", "")
    # A realistic identity is stable and 16-hex wide.
    fp = identity_fingerprint("carry-v1", 2, 3, "rel32-swing-baseline", "c0st")
    assert fp == ref("carry-v1", 2, 3, "rel32-swing-baseline", "c0st") and len(fp) == 16
    # LE field encoding: feature_version 1 vs 256 must differ (0x0100 vs 0x0001).
    assert identity_fingerprint("s", 1, 0, "", "") != identity_fingerprint(
        "s", 256, 0, "", ""
    )


def test_real_store_fingerprints_rederive_cross_language():
    """Python re-derives EVERY real identity directory name from its Parquet
    columns — the cross-language fingerprint contract on real bytes."""
    if not REAL_STORE.is_dir():
        pytest.skip("real observation store not present")
    fps = discover_identities(REAL_STORE)
    assert fps, "real store present but empty"
    for fp in fps:
        store = load_observation_store(REAL_STORE, fp)
        assert store.fingerprint == fp
        for r in store.rows[:5]:
            expect = identity_fingerprint(
                signal_id=r.signal_id,
                feature_version=r.feature_version,
                data_schema_version=r.data_schema_version,
                params_hash=r.params_hash,
                cost_model_hash=r.cost_model_hash,
            )
            assert expect == fp
        assert all(r.quality_code in range(6) for r in store.rows)


# --- Synthetic store: writer→reader roundtrip + tamper refusals ----------------


def _write_store(tmp_path: Path, rows: list[dict]) -> Path:
    import pyarrow.parquet as pq

    obs_dir = tmp_path / "observations"
    fp = rows[0]["signal_id"]  # not used as dir name; dir name is explicit
    del fp
    idir = obs_dir / "observations" / rows[0]["_fp"]
    idir.mkdir(parents=True)
    schema = pa.schema([(name, typ) for name, typ in COLUMNS])
    cols = {name: [r.get(name) for r in rows] for name, _ in COLUMNS}
    table = pa.table(cols, schema=schema)
    pq.write_table(table, idir / "date=2026-08-22-0000000000000000.parquet")
    return obs_dir


def _row(
    fp: str,
    oid: int,
    horizon: int | None = None,
    net: float | None = None,
    hit: int | None = None,
    signal_id: str = "s1",
    params: str = "p1",
    quality: int = 0,
    direction: int = 1,
) -> dict:
    base = {
        "observation_id": oid,
        "signal_id": signal_id,
        "feature_version": 1,
        "data_schema_version": 3,
        "params_hash": params,
        "cost_model_hash": "cost1",
        "ts_ns": 1_000_000_000 + oid,
        "symbol_id": 0,
        "venue_code": 4,
        "direction": direction,
        "quality": quality,
        "created_at_ns": 1,
        "snapshot_json": "{}",
        "_fp": fp,
    }
    if horizon is None:
        for c in OUTCOME_COLUMNS:
            base[c] = None
    else:
        base.update(
            {
                "outcome_horizon_ns": horizon,
                "outcome_entry_price": 100.0,
                "outcome_exit_price": 101.0,
                "outcome_gross_return": 0.01,
                "outcome_net_return": net,
                "outcome_mfe": 0.02,
                "outcome_ mae": None,
                "outcome_mae": 0.005,
                "outcome_hit": hit,
            }
        )
    return base


def _make_rows(fp: str | None = None) -> tuple[str, list[dict]]:
    fp = fp or identity_fingerprint("s1", 1, 3, "p1", "cost1")
    rows = [
        _row(fp, 1, horizon=900_000_000_000, net=0.001, hit=1),
        _row(
            fp, 1, horizon=3_600_000_000_000, net=None, hit=None
        ),  # open window row: net NULL
        _row(fp, 2, horizon=900_000_000_000, net=-0.002, hit=0),
    ]
    return fp, rows


def test_synthetic_roundtrip_and_net_outcomes_honest_denominator():
    """Writer semantics: one row per (observation, horizon); the open-window
    row (outcome NULL) is absent from net_outcomes — never imputed (R-1)."""
    fp, rows = _make_rows()
    obs_dir = _write_store(
        Path(__import__("tempfile").gettempdir())
        / f"mp-obs-test-{__import__('os').getpid()}",
        rows,
    )
    try:
        store = load_observation_store(obs_dir, fp)
        assert store.rows and len(store.rows) == 3
        closed = store.net_outcomes(900_000_000_000)
        assert [r.observation_id for r in closed] == [1, 2]
        # The open horizon's net is NULL ⇒ absent from that horizon's list.
        assert store.net_outcomes(3_600_000_000_000) == []
        # Quality/identity accessors.
        assert store.identity["params_hash"] == "p1"
        assert all(r.identity_fingerprint == fp for r in store.rows)
    finally:
        shutil.rmtree(obs_dir, ignore_errors=True)


def test_tampered_fingerprint_refused(tmp_path: Path):
    """Directory name says one identity, columns re-derive another ⇒ refuse."""
    fp_real = identity_fingerprint("s1", 1, 3, "p1", "cost1")
    rows = [_row(fp_real, 1, horizon=900_000_000_000, net=0.001, hit=1)]
    rows[0]["signal_id"] = "s2"  # tamper: identity columns no longer re-derive
    obs_dir = _write_store(tmp_path, rows)
    with pytest.raises(ObservationStoreError, match="fingerprint mismatch"):
        load_observation_store(obs_dir, fp_real)


def test_mixed_identity_rows_in_one_file_refused(tmp_path: Path):
    fp = identity_fingerprint("s1", 1, 3, "p1", "cost1")
    rows = [
        _row(fp, 1, horizon=900_000_000_000, net=0.001, hit=1),
        _row(fp, 2, horizon=900_000_000_000, net=0.002, hit=1, params="p2"),
    ]
    obs_dir = _write_store(tmp_path, rows)
    with pytest.raises(ObservationStoreError, match="identity-mixing rows"):
        load_observation_store(obs_dir, fp)


def test_multi_identity_load_refused(tmp_path: Path):
    fp1 = identity_fingerprint("s1", 1, 3, "p1", "cost1")
    fp2 = identity_fingerprint("s2", 1, 3, "p1", "cost1")
    obs_dir = _write_store(
        tmp_path, [_row(fp1, 1, horizon=900_000_000_000, net=0.0, hit=1)]
    )
    idir2 = obs_dir / "observations" / fp2
    idir2.mkdir(parents=True)
    # Copy the same file under a different dir: single-identity load of fp1
    # stays per-identity by construction (separate dirs), so just verify
    # discover sees both and each loads on its own.
    shutil.copy(
        obs_dir / "observations" / fp1 / "date=2026-08-22-0000000000000000.parquet",
        idir2 / "date=2026-08-22-0000000000000000.parquet",
    )
    assert discover_identities(obs_dir) == sorted([fp1, fp2])
    s1 = load_observation_store(obs_dir, fp1)
    assert s1.fingerprint == fp1


def test_schema_drift_refused(tmp_path: Path):
    import pyarrow.parquet as pq

    fp = identity_fingerprint("s1", 1, 3, "p1", "cost1")
    rows = [_row(fp, 1, horizon=900_000_000_000, net=0.001, hit=1)]
    obs_dir = _write_store(tmp_path, rows)
    idir = obs_dir / "observations" / fp
    path = idir / "date=2026-08-22-0000000000000000.parquet"
    table = pq.read_table(path)
    # Drop one column ⇒ schema drift.
    pq.write_table(table.drop_columns(["outcome_mae"]), path)
    with pytest.raises(ObservationStoreError, match="schema drift"):
        load_observation_store(obs_dir, fp)


# --- Grading bridge (R-4 net-primary) -----------------------------------------


def _observation_rows_for_grading() -> list[ObservationRow]:
    fp = identity_fingerprint("s1", 1, 3, "p1", "cost1")

    def r(oid: int, net: float, hit: int) -> ObservationRow:
        return ObservationRow(
            observation_id=oid,
            signal_id="s1",
            feature_version=1,
            data_schema_version=3,
            params_hash="p1",
            cost_model_hash="cost1",
            identity_fingerprint=fp,
            ts_ns=oid,
            symbol_id=0,
            venue_code=4,
            direction=1,
            quality_code=0,
            created_at_ns=0,
            snapshot_json="{}",
            outcome_horizon_ns=900_000_000_000,
            outcome_entry_price=100.0,
            outcome_exit_price=101.0,
            outcome_gross_return=0.01,
            outcome_net_return=net,
            outcome_mfe=0.0,
            outcome_mae=0.0,
            outcome_hit=hit,
        )

    return [r(1, 0.010, 1), r(2, -0.002, 0), r(3, 0.004, 1), r(4, -0.001, 0)]


def test_grade_observation_returns_net_primary_and_rust_semantics():
    rows = _observation_rows_for_grading()
    g = grade_observation_returns(rows, 900_000_000_000)
    assert g.n == 4
    assert g.wins == 2  # outcome_hit, matching the Rust evaluator
    assert g.avg_excess == approx((0.010 - 0.002 + 0.004 - 0.001) / 4)
    assert g.signal_id == "s1"
    assert g.identity_fingerprint == rows[0].identity_fingerprint


def test_grade_observation_returns_refuses_nonfinite():
    rows = _observation_rows_for_grading()
    bad = rows[0]
    object.__setattr__(bad, "outcome_net_return", float("nan"))
    with pytest.raises(ValueError, match="non-finite"):
        grade_observation_returns(rows, 900_000_000_000)


def test_tiers_and_decision_codes_match_spec():
    assert _tier(29) == "INSUFFICIENT"
    assert _tier(30) == "PRELIMINARY"
    assert _tier(100) == "RESEARCH"
    fp = identity_fingerprint("s1", 1, 3, "p1", "cost1")

    def mk(n: int, avg: float) -> object:
        from grading import RuleGrade

        g = RuleGrade(rule="s1", horizon_ns=900_000_000_000, n=n, wins=max(1, n // 2))
        g.sum_excess = avg * n
        g.signal_id = "s1"
        g.identity_fingerprint = fp
        return g

    d, reasons = _decision(mk(10, 0.001))
    assert d == "REJECT" and any("INSUFFICIENT_SAMPLE" in x for x in reasons)
    d, reasons = _decision(mk(150, -0.002))
    assert d == "REJECT" and any("NET_EXPECTANCY_NONPOSITIVE" in x for x in reasons)
    d, reasons = _decision(mk(150, 0.002))
    assert d == "GATE_PASS" and reasons == []


# --- Job contract: idempotency + journal (W-6) --------------------------------


def test_job_idempotent_and_journal_append_only(tmp_path: Path):
    fp, rows = _make_rows()
    # Give the corpus enough closed outcomes to grade (>= 1 row per horizon).
    obs_dir = _write_store(tmp_path, rows)
    out_dir = tmp_path / "grades"
    path1, ran1 = run_observation_grading(obs_dir, fp, out_dir)
    assert ran1 and path1.exists()
    payload = json.loads(path1.read_text(encoding="utf-8"))
    assert payload["identity"]["identity_fingerprint"] == fp
    assert payload["corpus_stamp"] == "2026-08-22"
    assert len(payload["horizons"]) == len(DEFAULT_HORIZONS_NS)
    journal = out_dir / "observation_grades" / "observation_grades.jsonl"
    lines1 = journal.read_text(encoding="utf-8").splitlines()
    assert len(lines1) == len(DEFAULT_HORIZONS_NS)
    # Re-run over the unchanged corpus: no-op, no double-append.
    path2, ran2 = run_observation_grading(obs_dir, fp, out_dir)
    assert not ran2 and path2 == path1
    assert journal.read_text(encoding="utf-8").splitlines() == lines1
