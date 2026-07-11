"""Daily-brief job (RES-5/6/7): compose the grounded morning brief from
structured inputs, enforce the grounding contract, and archive it.

The LLM *prose* is drafted by the `mp-llm` crate (Rust, nine providers); this
module owns the deterministic, testable contract around it:

- RES-5: the brief has the fixed sections, and a missing input section renders
  an explicit "no data" line — never invented content. Failure to generate a
  brief returns a P3 alert, not silence.
- RES-6: every brief is archived with its input-bundle hash + prompt version +
  model id + output, so a brief is reproducible or it doesn't ship.
- RES-7: the archive writer is confined to the briefs directory — it cannot
  write to `risk.toml`, funnel state, or any order path (a code-level guard;
  the process-user permission is the deployment backstop).

The `verify_grounded` numeric-subset check is applied to ANY brief text
(template draft or real LLM output) before it ships: every number in the brief
must appear in the input bundle.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path

# The fixed sections, in order (RES-5). Mirrors research/prompts/daily-brief.md.
SECTIONS = ["Regime", "Flows worth knowing", "Your book", "Data health", "Watch today"]

_NUMBER = re.compile(r"-?\d+(?:\.\d+)?")


def bundle_hash(bytes_: bytes) -> str:
    """FNV-1a 64-bit hex digest — the archival identity of an input bundle."""
    h = 0xCBF29CE484222325
    for b in bytes_:
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


@dataclass(frozen=True)
class InputBundle:
    """The exact structured inputs fed to the brief, plus their hash."""

    canonical: str
    hash: str

    @staticmethod
    def of(inputs: dict) -> "InputBundle":
        canonical = json.dumps(inputs, sort_keys=True, separators=(",", ":"))
        return InputBundle(canonical=canonical, hash=bundle_hash(canonical.encode()))


def numeric_tokens(text: str) -> set[str]:
    """Every numeric token in `text` (normalized so 1.50 == 1.5)."""
    out = set()
    for m in _NUMBER.findall(text):
        out.add(_norm_num(m))
    return out


def _norm_num(s: str) -> str:
    try:
        f = float(s)
        return f"{f:g}"
    except ValueError:
        return s


def _numbers_in(obj) -> set[str]:
    """All numbers appearing anywhere in the input structure (normalized)."""
    out: set[str] = set()
    if isinstance(obj, bool):
        return out
    if isinstance(obj, (int, float)):
        out.add(_norm_num(str(obj)))
    elif isinstance(obj, str):
        for m in _NUMBER.findall(obj):
            out.add(_norm_num(m))
    elif isinstance(obj, dict):
        for v in obj.values():
            out |= _numbers_in(v)
    elif isinstance(obj, (list, tuple)):
        for v in obj:
            out |= _numbers_in(v)
    return out


def verify_grounded(brief: str, inputs: dict) -> list[str]:
    """Return the numeric tokens in `brief` that do NOT appear in `inputs`
    (RES-5/6 grounding guard). Empty ⇒ every number is grounded. Applied to
    any brief text before it ships, template draft or LLM output alike.
    """
    allowed = _numbers_in(inputs)
    return sorted(t for t in numeric_tokens(brief) if t not in allowed)


def render_brief(inputs: dict, prompt_version: str = "brief-v1") -> str:
    """Deterministic grounded draft with the fixed sections (RES-5). A section
    whose input is missing/empty renders exactly "no data" — never invented
    content. This stands in for the LLM prose in tests; the real job swaps in
    `mp-llm` output and runs it through `verify_grounded` before shipping.
    """
    # The prompt version is archived with the record (RES-6), not embedded in
    # the brief body — an inline "v1" would read as an ungrounded number.
    _ = prompt_version
    lines = ["# Daily Brief", ""]
    for section in SECTIONS:
        lines.append(f"## {section}")
        body = inputs.get(section)
        if not body:
            lines.append("no data")
        elif isinstance(body, list):
            lines.extend(f"- {item}" for item in body)
        else:
            lines.append(str(body))
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


@dataclass(frozen=True)
class ArchiveRecord:
    """The append-only archive entry for one brief (RES-6)."""

    bundle_hash: str
    prompt_version: str
    model_id: str
    output: str

    def to_json(self) -> str:
        return json.dumps(
            {
                "bundle_hash": self.bundle_hash,
                "prompt_version": self.prompt_version,
                "model_id": self.model_id,
                "output": self.output,
            },
            sort_keys=True,
        )


class PermissionRefused(Exception):
    """Raised when the brief job tries to write outside its briefs dir (RES-7)."""


def _confined_path(briefs_dir: Path, name: str) -> Path:
    """Resolve `name` under `briefs_dir`, refusing any escape (RES-7). The
    brief job may write ONLY inside the briefs directory — never `risk.toml`,
    funnel state, or an order path."""
    briefs_dir = briefs_dir.resolve()
    target = (briefs_dir / name).resolve()
    if briefs_dir != target and briefs_dir not in target.parents:
        raise PermissionRefused(f"refusing to write outside briefs dir: {name}")
    return target


def archive_brief(briefs_dir: Path, record: ArchiveRecord, name: str = "briefs.jsonl") -> Path:
    """Append the brief record to the briefs archive (RES-6, append-only W-6).
    Confined to `briefs_dir` (RES-7)."""
    path = _confined_path(Path(briefs_dir), name)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as f:
        f.write(record.to_json() + "\n")
    return path


def generate_or_alert(
    inputs: dict, model_id: str, prompt_version: str = "brief-v1"
) -> tuple[str | None, str | None]:
    """Produce a grounded brief or a P3 alert — never silence (RES-5). Returns
    `(brief, None)` on success, or `(None, "P3: ...")` if generation fails or a
    number in the draft is ungrounded (which would be a grounding violation)."""
    try:
        brief = render_brief(inputs, prompt_version)
    except Exception as e:  # pragma: no cover - defensive
        return None, f"P3: brief generation failed: {e}"
    ungrounded = verify_grounded(brief, inputs)
    if ungrounded:
        return None, f"P3: brief has ungrounded numbers {ungrounded}"
    return brief, None
