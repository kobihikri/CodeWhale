#!/usr/bin/env python3
"""Reject `workflow_dispatch` inputs that can never be supplied.

`workflow_dispatch` reads its input schema from the workflow file **at the ref
being dispatched**. A workflow that hard-requires being dispatched from a tag
ref therefore only ever exposes the inputs that existed *in that tag*. Adding a
new input to such a workflow is dead on arrival for every tag cut before the
change — which, for a recovery option, excludes every tag that could ever need
recovering.

That is what #4801 shipped and #4802 had to revert:

    HTTP 422: Unexpected inputs provided: ["republish_channels"]

`actionlint` passed it. It validates syntax, expressions, and action refs; it
has no concept of "this input can never be supplied." This check adds that one
invariant, and nothing else.

Ref-pinning is **detected, not declared** — there is no allowlist to maintain
and no way for a new workflow to opt out of the check by forgetting to opt in.
A workflow counts as ref-pinned when its body compares `GITHUB_REF` /
`github.ref` against a `refs/tags/` value, which is exactly how `release.yml`
enforces it:

    if [[ "${GITHUB_REF}" != "refs/tags/${tag}" ]]; then
      echo "::error::Dispatch release.yml from --ref ${tag}, not ${GITHUB_REF}."

The failure message names the fix rather than the symptom: recovery paths
belong in a workflow dispatchable from the default branch, taking the tag as an
input instead of as the dispatch ref.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_DIR = Path(".github/workflows")

# `GITHUB_REF`/`github.ref`/`GITHUB_REF_NAME` tested against a tag ref. Covers
# the shell form (`"${GITHUB_REF}" != "refs/tags/..."`) and the expression form
# (`github.ref != 'refs/tags/...'`, `startsWith(github.ref, 'refs/tags/')`).
REF_TAG_TEST_RE = re.compile(
    # The ref under test: shell env, workflow expression, or the `context.ref`
    # form used inside actions/github-script.
    r"(?:GITHUB_REF(?:_NAME)?|github\.ref(?:_name)?|context\.ref)"
    r"(?:[^\n]{0,80}?)"  # operator / function plumbing on the same line
    r"refs/tags/",
    re.IGNORECASE,
)

# A ref-vs-tag test alone is NOT ref-pinning. `sync-cnb.yml` branches on
# tag-ness and handles both cases, so it stays dispatchable from the default
# branch and a new input there is perfectly usable. What strands an input is a
# workflow that *refuses to run* off-tag, so the test must be followed by a
# hard stop. Requiring the refusal keeps the check precise: a lint that
# false-positives is a lint someone deletes.
REFUSAL_RE = re.compile(
    r"::error|exit\s+[1-9]|\bfailure\(\)|core\.setFailed",
    re.IGNORECASE,
)

# How many lines after the ref test to look for the refusal.
REFUSAL_WINDOW = 4


def run(args: list[str], cwd: Path) -> str | None:
    """Return stdout, or None when the command fails (missing ref, new file)."""
    try:
        done = subprocess.run(
            args, cwd=cwd, capture_output=True, text=True, check=True
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None
    return done.stdout


def dispatch_inputs(text: str) -> set[str]:
    """Names declared under `on.workflow_dispatch.inputs`.

    Parsed with indentation rather than a YAML library so the check has no
    dependency to install in CI. The shape being read is fixed and shallow:
    `inputs:` under `workflow_dispatch:`, one mapping key per input.
    """
    lines = text.splitlines()
    names: set[str] = set()

    dispatch_indent: int | None = None
    inputs_indent: int | None = None

    for raw in lines:
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        indent = len(raw) - len(raw.lstrip())
        stripped = raw.strip()

        if dispatch_indent is None:
            if stripped.startswith("workflow_dispatch:"):
                dispatch_indent = indent
            continue

        # Left the workflow_dispatch block entirely.
        if indent <= dispatch_indent:
            if inputs_indent is not None:
                break
            dispatch_indent = None
            if stripped.startswith("workflow_dispatch:"):
                dispatch_indent = indent
            continue

        if inputs_indent is None:
            if stripped.startswith("inputs:"):
                inputs_indent = indent
            continue

        if indent <= inputs_indent:
            break

        # First key level under `inputs:` is an input name.
        if indent == inputs_indent + 2 and stripped.endswith(":"):
            names.add(stripped[:-1].strip())

    return names


def is_ref_pinned(text: str) -> bool:
    """True when the workflow refuses to run unless dispatched from a tag ref.

    Both halves are required: a comparison of the ref against `refs/tags/`, and
    a refusal (`::error`, a non-zero `exit`, `core.setFailed`) within the next
    few lines. A workflow that merely *branches* on tag-ness is not ref-pinned.
    """
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if not REF_TAG_TEST_RE.search(line):
            continue
        window = lines[index : index + 1 + REFUSAL_WINDOW]
        if any(REFUSAL_RE.search(candidate) for candidate in window):
            return True
    return False


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base",
        default="origin/main",
        help="Revision to diff against (default: origin/main).",
    )
    parser.add_argument(
        "--root", default=str(ROOT), help="Repository root (default: repo root)."
    )
    args = parser.parse_args()
    root = Path(args.root).resolve()

    workflow_dir = root / WORKFLOW_DIR
    if not workflow_dir.is_dir():
        print(f"no {WORKFLOW_DIR} directory under {root}; nothing to check")
        return 0

    merge_base = run(["git", "merge-base", args.base, "HEAD"], root)
    base_rev = merge_base.strip() if merge_base else args.base

    failures: list[str] = []
    checked = 0

    for path in sorted(workflow_dir.glob("*.yml")) + sorted(
        workflow_dir.glob("*.yaml")
    ):
        text = path.read_text(encoding="utf-8")
        if "workflow_dispatch:" not in text or not is_ref_pinned(text):
            continue

        checked += 1
        rel = path.relative_to(root).as_posix()
        current = dispatch_inputs(text)

        previous_text = run(["git", "show", f"{base_rev}:{rel}"], root)
        if previous_text is None:
            # New ref-pinned workflow: no baseline to compare, and its inputs
            # ship with it, so nothing is stranded.
            continue

        added = sorted(current - dispatch_inputs(previous_text))
        if added:
            failures.append(
                f"{rel}: new workflow_dispatch input(s) {', '.join(added)}\n"
                f"    {rel} is ref-pinned — it requires being dispatched from a "
                f"refs/tags/ ref.\n"
                f"    workflow_dispatch reads its input schema from the workflow "
                f"file at that ref, so a\n"
                f"    new input is unusable on every tag that already exists, "
                f"including every tag that\n"
                f"    could need it. Put recovery in a workflow dispatchable "
                f"from the default branch,\n"
                f"    taking the tag as an input instead of as the dispatch ref."
            )

    if failures:
        print("Unusable workflow_dispatch input(s) detected:\n", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}\n", file=sys.stderr)
        return 1

    print(f"ok: {checked} ref-pinned workflow(s) gained no unusable dispatch inputs")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
