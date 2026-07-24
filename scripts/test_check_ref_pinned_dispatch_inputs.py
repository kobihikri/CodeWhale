#!/usr/bin/env python3
"""Tests for check-ref-pinned-dispatch-inputs.py.

The interesting cases are the two ways this check could be useless: missing the
bug it was written for (#4801's `republish_channels`), or flagging a workflow
that is merely tag-aware, which would get the lint deleted the first time it
blocked a legitimate PR.
"""

from __future__ import annotations

import importlib.util
import subprocess
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).resolve().parent / "check-ref-pinned-dispatch-inputs.py"
_spec = importlib.util.spec_from_file_location("ref_pin_check", MODULE_PATH)
assert _spec and _spec.loader
check = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(check)


# release.yml: refuses to run unless dispatched from the tag.
REF_PINNED = """\
on:
  push:
    tags: ['v*']
  workflow_dispatch:
    inputs:
      version:
        description: 'Release version'
        required: true
        type: string
jobs:
  resolve:
    steps:
      - run: |
          if [[ "${GITHUB_REF}" != "refs/tags/${tag}" ]]; then
            echo "::error::Dispatch release.yml from --ref ${tag}, not ${GITHUB_REF}." >&2
            exit 1
          fi
"""

REF_PINNED_PLUS_INPUT = REF_PINNED.replace(
    """        type: string
""",
    """        type: string
      republish_channels:
        description: 'Recovery'
        required: false
        default: false
        type: boolean
""",
)

# sync-cnb.yml shape: branches on tag-ness, handles both, never refuses.
TAG_AWARE_NOT_PINNED = """\
on:
  workflow_dispatch:
    inputs:
      dry_run:
        type: boolean
jobs:
  sync:
    steps:
      - run: |
          if [[ "${GITHUB_REF}" == refs/tags/* ]]; then
            TAG="${GITHUB_REF#refs/tags/}"
            push_with_retry "tag ${TAG}"
          else
            push_with_retry "branch"
          fi
"""


class RefPinDetection(unittest.TestCase):
    def test_refusal_after_tag_test_is_ref_pinned(self):
        self.assertTrue(check.is_ref_pinned(REF_PINNED))

    def test_branching_on_tag_is_not_ref_pinned(self):
        # The #4801 fix must not fire on workflows that merely read the ref.
        # sync-cnb.yml stays dispatchable from the default branch, so an input
        # added there is perfectly usable.
        self.assertFalse(check.is_ref_pinned(TAG_AWARE_NOT_PINNED))

    def test_expression_form_with_setfailed(self):
        text = """\
on:
  workflow_dispatch:
    inputs:
      version:
        type: string
jobs:
  guard:
    steps:
      - uses: actions/github-script@v7
        with:
          script: |
            if (!context.ref.startsWith('refs/tags/')) {
              core.setFailed('dispatch from the tag');
            }
"""
        self.assertTrue(check.is_ref_pinned(text))


class InputParsing(unittest.TestCase):
    def test_reads_dispatch_input_names(self):
        self.assertEqual(check.dispatch_inputs(REF_PINNED), {"version"})

    def test_reads_added_input(self):
        self.assertEqual(
            check.dispatch_inputs(REF_PINNED_PLUS_INPUT),
            {"version", "republish_channels"},
        )

    def test_ignores_job_level_keys_outside_dispatch(self):
        # `jobs:` and step keys must not be mistaken for input names.
        self.assertNotIn("jobs", check.dispatch_inputs(REF_PINNED))
        self.assertNotIn("resolve", check.dispatch_inputs(REF_PINNED))

    def test_no_dispatch_block(self):
        self.assertEqual(check.dispatch_inputs("on:\n  push:\n    branches: [main]\n"), set())


class EndToEnd(unittest.TestCase):
    """Drive the real script over a throwaway git repo."""

    def _repo(self, tmp: Path) -> None:
        def git(*args: str) -> None:
            subprocess.run(
                ["git", *args], cwd=tmp, check=True, capture_output=True, text=True
            )

        git("init", "-q", "-b", "main")
        git("config", "user.email", "t@example.com")
        git("config", "user.name", "t")
        (tmp / ".github" / "workflows").mkdir(parents=True)
        self.git = git

    def _run(self, tmp: Path, base: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["python3", str(MODULE_PATH), "--base", base, "--root", str(tmp)],
            capture_output=True,
            text=True,
        )

    def test_added_input_on_ref_pinned_workflow_fails(self):
        import tempfile

        with tempfile.TemporaryDirectory() as raw:
            tmp = Path(raw)
            self._repo(tmp)
            wf = tmp / ".github" / "workflows" / "release.yml"
            wf.write_text(REF_PINNED)
            self.git("add", "-A")
            self.git("commit", "-qm", "base")

            wf.write_text(REF_PINNED_PLUS_INPUT)
            self.git("add", "-A")
            self.git("commit", "-qm", "add recovery input")

            result = self._run(tmp, "HEAD~1")
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("republish_channels", result.stderr)
            # The message must name the fix, not just the symptom.
            self.assertIn("dispatchable from the default branch", result.stderr)

    def test_added_input_on_unpinned_workflow_passes(self):
        import tempfile

        with tempfile.TemporaryDirectory() as raw:
            tmp = Path(raw)
            self._repo(tmp)
            wf = tmp / ".github" / "workflows" / "sync.yml"
            wf.write_text(TAG_AWARE_NOT_PINNED)
            self.git("add", "-A")
            self.git("commit", "-qm", "base")

            wf.write_text(TAG_AWARE_NOT_PINNED.replace(
                "      dry_run:\n        type: boolean\n",
                "      dry_run:\n        type: boolean\n      extra:\n        type: string\n",
            ))
            self.git("add", "-A")
            self.git("commit", "-qm", "add input")

            result = self._run(tmp, "HEAD~1")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_unchanged_ref_pinned_workflow_passes(self):
        import tempfile

        with tempfile.TemporaryDirectory() as raw:
            tmp = Path(raw)
            self._repo(tmp)
            (tmp / ".github" / "workflows" / "release.yml").write_text(REF_PINNED)
            self.git("add", "-A")
            self.git("commit", "-qm", "base")
            self.git("commit", "-qm", "empty", "--allow-empty")

            result = self._run(tmp, "HEAD~1")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
