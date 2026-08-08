"""Leverage-tier calibration (spec 029 LIQ-11): consume the `whale_study
--leverage-calibration` report — the recorded spec 028 real leverage
distribution, notional-weighted onto the configured tier set — and render the
`[liq_est_bands]` TOML that replaces the documented assumption weights.

Pure stdlib and deterministic (same contract as `band_accuracy.py`): parsing
is fail-closed (a malformed report raises ``ValueError``), and the TOML
section is a pure function of the run, so the operator's `features.toml`
override and the journaled evidence can never drift.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

WEIGHT_EPSILON = 1e-3  # Σ weights ≈ 1.0 (the model's OI-spread invariant)


@dataclass(frozen=True)
class Tier:
    """One calibrated tier bucket from the report."""

    leverage: float
    weight: float
    count: int
    notional: float


@dataclass(frozen=True)
class CalibrationRun:
    """One parsed ``whale_study --leverage-calibration --json`` report."""

    run_id: str | None
    git_sha: str | None
    config_hash: str | None
    data_from_ns: int
    data_to_ns: int
    n: int
    positions_seen: int
    total_notional: float
    maintenance_buffer: float
    sum_weights: float
    tiers: tuple[Tier, ...]


def parse_report(obj: Any) -> CalibrationRun:
    """Validate and parse the calibration report dict.

    Fail-closed (CONV-8): the report must be a `leverage_calibration` study
    with a well-typed `tiers` array and metrics, or a ``ValueError`` is
    raised — a truncated/corrupt report must never feed a config override.
    Provenance fields (``run_id``/``git_sha``/``config_hash``/data range) are
    echoes and optional.
    """
    if not isinstance(obj, dict):
        raise ValueError(f"calibration report is not an object: {type(obj).__name__}")
    if obj.get("study") != "leverage_calibration":
        raise ValueError(
            f"not a leverage_calibration report: study={obj.get('study')!r}"
        )

    tiers = obj.get("tiers")
    if not isinstance(tiers, list) or not tiers:
        raise ValueError("report 'tiers' must be a non-empty array")
    parsed: list[Tier] = []
    for row in tiers:
        if not isinstance(row, dict):
            raise ValueError(f"tier row is not an object: {row!r}")
        try:
            parsed.append(
                Tier(
                    leverage=_as_float(row["leverage"], "tiers[].leverage"),
                    weight=_as_float(row["weight"], "tiers[].weight"),
                    count=_as_int(row["count"], "tiers[].count"),
                    notional=_as_float(row["notional"], "tiers[].notional"),
                )
            )
        except KeyError as e:
            raise ValueError(f"tier row missing {e.args[0]}") from None

    return CalibrationRun(
        run_id=_as_optional_str(obj.get("run_id")),
        git_sha=_as_optional_str(obj.get("git_sha")),
        config_hash=_as_optional_str(obj.get("config_hash")),
        data_from_ns=_as_int(obj.get("data_from_ns", 0), "data_from_ns"),
        data_to_ns=_as_int(obj.get("data_to_ns", 0), "data_to_ns"),
        n=_as_int(obj.get("n", 0), "n"),
        positions_seen=_as_int(obj.get("positions_seen", 0), "positions_seen"),
        total_notional=_as_float(obj.get("total_notional", 0.0), "total_notional"),
        maintenance_buffer=_as_float(
            obj.get("maintenance_buffer", 0.0), "maintenance_buffer"
        ),
        sum_weights=_as_float(obj.get("sum_weights", 0.0), "sum_weights"),
        tiers=tuple(parsed),
    )


def render_toml(run: CalibrationRun) -> str:
    """The `[liq_est_bands]` TOML section carrying the calibrated weights.

    Deterministic; parses as a standalone `features.toml` (other sections
    default, LIQ-7 `deny_unknown_fields`). All configured tiers are emitted —
    a zero-weight tier still shapes the nearest cascade level (LIQ-2) — with
    weights rounded to 8 significant figures for readability.
    """
    header = (
        "[liq_est_bands]\n"
        "# Calibrated from recorded spec 028 real leverage distribution (spec 029 LIQ-11):\n"
        f"# n={run.n} positions, total_notional={_toml_float(run.total_notional)}"
        + (f", config_hash={run.config_hash}" if run.config_hash else "")
        + "\n"
    )
    body = [f"maintenance_buffer = {_toml_float(run.maintenance_buffer)}"]
    for t in run.tiers:
        body.append("[[liq_est_bands.leverage_tiers]]")
        body.append(f"leverage = {_toml_float(t.leverage)}")
        body.append(f"weight = {_toml_float(t.weight)}")
    return header + "\n".join(body) + "\n"


def _toml_float(x: float) -> str:
    """Shortest readable TOML float — always with a decimal point (`1.0`)."""
    s = format(x, ".8g")
    if "e" not in s and "." not in s:
        s += ".0"
    return s


def _as_int(value: Any, key: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"report {key!r} is not an int: {value!r}")
    return value


def _as_float(value: Any, key: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"report {key!r} is not a number: {value!r}")
    return float(value)


def _as_optional_str(value: Any) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        raise ValueError(f"report provenance field is not a string: {value!r}")
    return value
