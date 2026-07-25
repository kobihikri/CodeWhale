#!/usr/bin/env python3
"""Regression tests for scripts/check-locale-matrix.py.

Every test asserts the gate *fails* on a specific kind of drift. A locale gate
that only proves the happy path is the green check that guards nothing.
"""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "check-locale-matrix.py"

SPEC = importlib.util.spec_from_file_location("check_locale_matrix", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


REGISTRY = """
impl Locale {
    pub fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ja => "ja",
            Self::ZhHant => "zh-Hant",
        }
    }

    pub fn shipped_complete() -> &'static [Self] {
        &[
            Self::En,
            Self::Ja,
        ]
    }

    pub fn is_partial_pack(self) -> bool {
        matches!(self, Self::ZhHant)
    }
}
"""

MATRIX = """
## TUI locale packs

| Locale | Pack | Keys vs `en.json` | Status |
|--------|------|-------------------|--------|
| English | `crates/tui/locales/en.json` | 3 (source) | **shipped** |
| Japanese | `crates/tui/locales/ja.json` | 3 / 3 | **shipped** |
| Traditional Chinese | `crates/tui/locales/zh-Hant.json` | 1 / 3 | **partial** |
"""


def packs(**overrides: set[str]) -> dict[str, set[str]]:
    base = {
        "en": {"a", "b", "c"},
        "ja": {"a", "b", "c"},
        "zh-Hant": {"a"},
    }
    base.update(overrides)
    return base


def run(pack_map: dict[str, set[str]], matrix_text: str = MATRIX) -> list[str]:
    tags, complete, partial = mod.parse_registry(REGISTRY)
    matrix = mod.parse_matrix(matrix_text)
    return mod.check(pack_map, set(tags.values()), complete, partial, matrix)


class ParseTests(unittest.TestCase):
    def test_registry_parses_tags_complete_and_partial(self) -> None:
        tags, complete, partial = mod.parse_registry(REGISTRY)
        self.assertEqual(set(tags.values()), {"en", "ja", "zh-Hant"})
        self.assertEqual(complete, {"en", "ja"})
        self.assertEqual(partial, {"zh-Hant"})

    def test_missing_tui_table_is_a_hard_error(self) -> None:
        with self.assertRaises(mod.MatrixError):
            mod.parse_matrix("# Localization Matrix\n\nno table here\n")

    def test_real_repo_files_agree(self) -> None:
        self.assertEqual(mod.main([]), 0)


class DriftTests(unittest.TestCase):
    def test_clean_input_passes(self) -> None:
        self.assertEqual(run(packs()), [])

    def test_complete_pack_missing_a_key_fails(self) -> None:
        problems = run(packs(ja={"a", "b"}))
        self.assertTrue(any("missing key 'c'" in p for p in problems), problems)

    def test_complete_pack_with_an_extra_key_fails(self) -> None:
        problems = run(packs(ja={"a", "b", "c", "d"}))
        self.assertTrue(
            any("key 'd' that en.json does not define" in p for p in problems), problems
        )

    def test_english_gaining_a_key_fails_every_other_pack(self) -> None:
        problems = run(packs(en={"a", "b", "c", "d"}))
        self.assertTrue(any("ja.json" in p and "missing key 'd'" in p for p in problems), problems)

    def test_partial_pack_with_an_unknown_key_fails(self) -> None:
        problems = run(packs(**{"zh-Hant": {"a", "zzz"}}))
        self.assertTrue(any("absent from en.json" in p for p in problems), problems)

    def test_partial_pack_reaching_parity_must_be_promoted(self) -> None:
        problems = run(packs(**{"zh-Hant": {"a", "b", "c"}}))
        self.assertTrue(any("promote it out of is_partial_pack" in p for p in problems), problems)

    def test_pack_absent_from_the_matrix_fails(self) -> None:
        problems = run(packs(ru={"a", "b", "c"}))
        self.assertTrue(any("no TUI row" in p for p in problems), problems)
        self.assertTrue(any("registry never declares it" in p for p in problems), problems)

    def test_matrix_row_without_a_pack_fails(self) -> None:
        extra = MATRIX + "| Russian | `crates/tui/locales/ru.json` | 3 / 3 | **shipped** |\n"
        problems = run(packs(), extra)
        self.assertTrue(any("ru.json does not exist" in p for p in problems), problems)

    def test_stale_key_count_in_the_matrix_fails(self) -> None:
        stale = MATRIX.replace("| 1 / 3 | **partial** |", "| 2 / 3 | **partial** |")
        problems = run(packs(), stale)
        self.assertTrue(any("says zh-Hant has 2 keys" in p for p in problems), problems)

    def test_matrix_status_disagreeing_with_registry_fails(self) -> None:
        wrong = MATRIX.replace(
            "| 1 / 3 | **partial** |", "| 1 / 3 | **shipped** |"
        )
        problems = run(packs(), wrong)
        self.assertTrue(any("makes it 'partial'" in p for p in problems), problems)


class CliTests(unittest.TestCase):
    def test_cli_exits_nonzero_on_a_drifted_pack_tree(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            tmpdir = Path(tmp)
            pack_dir = tmpdir / "locales"
            pack_dir.mkdir()
            for tag, keys in packs(ja={"a", "b"}).items():
                (pack_dir / f"{tag}.json").write_text(
                    json.dumps({k: k for k in sorted(keys)}), encoding="utf-8"
                )
            registry = tmpdir / "localization.rs"
            registry.write_text(REGISTRY, encoding="utf-8")
            matrix = tmpdir / "LOCALIZATION.md"
            matrix.write_text(MATRIX, encoding="utf-8")

            code = mod.main(
                [
                    "--pack-dir",
                    str(pack_dir),
                    "--registry",
                    str(registry),
                    "--matrix",
                    str(matrix),
                ]
            )
        self.assertEqual(code, 1)


if __name__ == "__main__":
    unittest.main()
