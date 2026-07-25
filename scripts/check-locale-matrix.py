#!/usr/bin/env python3
"""Gate TUI locale-pack drift against the localization matrix.

Three surfaces have to agree or the matrix stops being a planning input:

1. the JSON packs under ``crates/tui/locales/``,
2. the Rust registry in ``crates/tui/src/localization.rs`` (``Locale::tag``,
   ``Locale::shipped_complete``, ``Locale::is_partial_pack``),
3. the **TUI locale packs** table in ``docs/LOCALIZATION.md``.

Failures this catches:

* a pack gains or loses a key relative to ``en.json`` while still claiming
  ``shipped``,
* a declared-partial pack holding keys English does not have (a typo'd key
  silently falls back to English forever),
* a pack file added or removed without touching the registry or the matrix,
* a matrix row whose key count or status no longer matches reality.

Exits non-zero with one line per problem.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_PACK_DIR = ROOT / "crates" / "tui" / "locales"
DEFAULT_REGISTRY = ROOT / "crates" / "tui" / "src" / "localization.rs"
DEFAULT_MATRIX = ROOT / "docs" / "LOCALIZATION.md"

SOURCE_TAG = "en"

TAG_ARM_RE = re.compile(r"Self::(\w+)\s*=>\s*\"([A-Za-z0-9-]+)\"")
FN_BODY_RE = r"fn\s+{name}\s*\([^)]*\)[^{{]*\{{(.*?)\n    \}}"
VARIANT_RE = re.compile(r"Self::(\w+)")
# | Japanese | `crates/tui/locales/ja.json` | 1123 / 1123 | **shipped** |
ROW_RE = re.compile(
    r"^\|[^|]*\|\s*`crates/tui/locales/(?P<tag>[A-Za-z0-9-]+)\.json`\s*\|"
    r"\s*(?P<keys>\d+)[^|]*\|\s*(?:\*\*)?(?P<status>[a-z]+)(?:\*\*)?\s*\|",
    re.MULTILINE,
)


class MatrixError(Exception):
    """Raised when an input file cannot be parsed at all."""


def _fn_body(source: str, name: str) -> str:
    match = re.search(FN_BODY_RE.format(name=name), source, re.DOTALL)
    if not match:
        raise MatrixError(f"could not find fn {name}() in the locale registry")
    return match.group(1)


def parse_registry(source: str) -> tuple[dict[str, str], set[str], set[str]]:
    """Return (variant -> tag, complete tags, partial tags)."""
    tags = dict(TAG_ARM_RE.findall(_fn_body(source, "tag")))
    if not tags:
        raise MatrixError("locale registry exposes no tag() arms")

    def tags_for(fn: str) -> set[str]:
        found = set()
        for variant in VARIANT_RE.findall(_fn_body(source, fn)):
            if variant not in tags:
                raise MatrixError(f"{fn}() names unknown locale variant {variant}")
            found.add(tags[variant])
        return found

    return tags, tags_for("shipped_complete"), tags_for("is_partial_pack")


def parse_matrix(text: str) -> dict[str, tuple[int, str]]:
    """Return tag -> (declared key count, declared status) from the TUI table."""
    rows = {
        m.group("tag"): (int(m.group("keys")), m.group("status"))
        for m in ROW_RE.finditer(text)
    }
    if not rows:
        raise MatrixError(
            "docs/LOCALIZATION.md has no TUI locale packs table "
            "(expected rows referencing `crates/tui/locales/<tag>.json`)"
        )
    return rows


def load_packs(pack_dir: Path) -> dict[str, set[str]]:
    packs: dict[str, set[str]] = {}
    for path in sorted(pack_dir.glob("*.json")):
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:  # pragma: no cover - trivial
            raise MatrixError(f"{path.name} is not valid JSON: {exc}") from exc
        if not isinstance(data, dict):
            raise MatrixError(f"{path.name} must be a JSON object of message keys")
        packs[path.stem] = set(data)
    if SOURCE_TAG not in packs:
        raise MatrixError(f"missing source pack {SOURCE_TAG}.json in {pack_dir}")
    return packs


def check(
    packs: dict[str, set[str]],
    registry_tags: set[str],
    complete: set[str],
    partial: set[str],
    matrix: dict[str, tuple[int, str]],
) -> list[str]:
    problems: list[str] = []
    english = packs[SOURCE_TAG]

    for missing in sorted(registry_tags - set(packs)):
        problems.append(f"registry declares locale {missing} but {missing}.json is absent")
    for extra in sorted(set(packs) - registry_tags):
        problems.append(f"{extra}.json ships but the Rust locale registry never declares it")
    for missing in sorted(set(packs) - set(matrix)):
        problems.append(f"{missing}.json ships but docs/LOCALIZATION.md has no TUI row for it")
    for extra in sorted(set(matrix) - set(packs)):
        problems.append(f"docs/LOCALIZATION.md lists TUI pack {extra} but {extra}.json does not exist")

    for tag in sorted(set(packs) & set(matrix)):
        declared_keys, declared_status = matrix[tag]
        actual = len(packs[tag])
        if declared_keys != actual:
            problems.append(
                f"docs/LOCALIZATION.md says {tag} has {declared_keys} keys; {tag}.json has {actual}"
            )
        expected_status = "partial" if tag in partial else "shipped"
        if declared_status != expected_status:
            problems.append(
                f"docs/LOCALIZATION.md marks {tag} '{declared_status}'; "
                f"the registry makes it '{expected_status}'"
            )

    for tag in sorted(complete):
        pack = packs.get(tag)
        if pack is None:
            continue
        if tag in partial:
            problems.append(f"{tag} is listed as both shipped_complete and is_partial_pack")
        for key in sorted(english - pack):
            problems.append(f"{tag}.json claims completeness but is missing key '{key}'")
        for key in sorted(pack - english):
            problems.append(f"{tag}.json has key '{key}' that en.json does not define")

    for tag in sorted(partial):
        pack = packs.get(tag)
        if pack is None:
            continue
        for key in sorted(pack - english):
            problems.append(
                f"partial pack {tag}.json has key '{key}' absent from en.json "
                "(it can never be reached; English fallback keys must exist upstream)"
            )
        if pack >= english:
            problems.append(
                f"{tag}.json now has full en.json parity; promote it out of is_partial_pack()"
            )

    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pack-dir", type=Path, default=DEFAULT_PACK_DIR)
    parser.add_argument("--registry", type=Path, default=DEFAULT_REGISTRY)
    parser.add_argument("--matrix", type=Path, default=DEFAULT_MATRIX)
    args = parser.parse_args(argv)

    try:
        packs = load_packs(args.pack_dir)
        tags, complete, partial = parse_registry(args.registry.read_text(encoding="utf-8"))
        matrix = parse_matrix(args.matrix.read_text(encoding="utf-8"))
    except (MatrixError, OSError) as exc:
        print(f"[check-locale-matrix] ERROR: {exc}", file=sys.stderr)
        return 2

    problems = check(packs, set(tags.values()), complete, partial, matrix)
    if problems:
        print("[check-locale-matrix] FAIL — locale drift detected:", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        print(
            "\nFix the pack, the registry in crates/tui/src/localization.rs, "
            "or the TUI table in docs/LOCALIZATION.md until all three agree.",
            file=sys.stderr,
        )
        return 1

    print(
        f"[check-locale-matrix] PASS — {len(packs)} TUI packs agree with the registry "
        f"and docs/LOCALIZATION.md ({len(packs[SOURCE_TAG])} source keys)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
