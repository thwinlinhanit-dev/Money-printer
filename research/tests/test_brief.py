"""RES-5/6/7 daily-brief job: fixed sections, grounding guard, archival,
and the write-confinement permission guard. Names embed the requirement id."""

import json

import pytest

from brief import (
    SECTIONS,
    ArchiveRecord,
    InputBundle,
    PermissionRefused,
    archive_brief,
    generate_or_alert,
    render_brief,
    verify_grounded,
)


def fixture_inputs():
    return {
        "Regime": "chop; vol regime unchanged in last 24h",
        "Flows worth knowing": ["funding z-score 2.1 on BTC", "OI up 3.5%"],
        "Your book": "flat",
        "Data health": "coverage 0.998, no gaps",
        "Watch today": ["liq cluster forming near 61000"],
    }


def test_res_5_brief_has_all_fixed_sections():
    md = render_brief(fixture_inputs())
    for section in SECTIONS:
        assert f"## {section}" in md, f"missing section {section}"


def test_res_5_missing_section_renders_no_data_not_invention():
    inputs = fixture_inputs()
    del inputs["Your book"]  # no live book today
    md = render_brief(inputs)
    # The section is still present, filled with an explicit "no data" line.
    assert "## Your book" in md
    book_block = md.split("## Your book", 1)[1].split("##", 1)[0]
    assert "no data" in book_block


def test_res_6_brief_quotes_only_input_numbers():
    inputs = fixture_inputs()
    md = render_brief(inputs)
    # Grounding guard: every number in the brief traces to the inputs.
    assert verify_grounded(md, inputs) == []


def test_res_6_ungrounded_number_is_detected():
    inputs = fixture_inputs()
    # A brief that invents a price the inputs never contained is caught.
    tampered = render_brief(inputs) + "\nInvented target: 99999.0\n"
    assert "99999" in verify_grounded(tampered, inputs)[0]


def test_res_5_generate_or_alert_returns_p3_on_ungrounded(monkeypatch):
    inputs = fixture_inputs()
    # Force an ungrounded draft by monkeypatching the renderer to inject a number.
    import brief as brief_mod

    monkeypatch.setattr(
        brief_mod, "render_brief", lambda i, v="brief-v1": "## Regime\nmade-up 42424242\n"
    )
    out, alert = generate_or_alert(inputs, model_id="claude-opus-4-8")
    assert out is None
    assert alert is not None and alert.startswith("P3")


def test_res_6_input_bundle_hash_is_deterministic():
    a = InputBundle.of(fixture_inputs())
    b = InputBundle.of(fixture_inputs())
    assert a.hash == b.hash
    # A changed input changes the hash.
    changed = fixture_inputs()
    changed["Your book"] = "long 0.1 BTC"
    assert InputBundle.of(changed).hash != a.hash


def test_res_6_archive_writes_reproducible_record(tmp_path):
    bundle = InputBundle.of(fixture_inputs())
    rec = ArchiveRecord(
        bundle_hash=bundle.hash,
        prompt_version="brief-v1",
        model_id="claude-opus-4-8",
        output=render_brief(fixture_inputs()),
    )
    path = archive_brief(tmp_path, rec)
    line = path.read_text(encoding="utf-8").strip()
    obj = json.loads(line)
    assert obj["bundle_hash"] == bundle.hash
    assert obj["model_id"] == "claude-opus-4-8"
    assert obj["prompt_version"] == "brief-v1"
    # Append-only: a second archive adds a line, never rewrites.
    archive_brief(tmp_path, rec)
    assert len(path.read_text(encoding="utf-8").strip().splitlines()) == 2


def test_res_7_archive_refuses_to_write_outside_briefs_dir(tmp_path):
    rec = ArchiveRecord("h", "brief-v1", "m", "o")
    # The brief job must not be able to write risk.toml or funnel state.
    with pytest.raises(PermissionRefused):
        archive_brief(tmp_path, rec, name="../risk.toml")
    with pytest.raises(PermissionRefused):
        archive_brief(tmp_path, rec, name="../../funnel/state.json")


def test_res_8_prompt_templates_carry_version_headers():
    import pathlib
    import re

    prompts = pathlib.Path(__file__).resolve().parents[1] / "prompts"
    files = list(prompts.glob("*.md"))
    assert files, "research/prompts must contain versioned templates (RES-8)"
    for f in files:
        text = f.read_text(encoding="utf-8")
        assert re.search(r"prompt-version:\s*\S+", text), f"{f.name} lacks a prompt-version header"
