#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Exercise the checked-in Fern preview step with a local Fern fixture."""

import os
from dataclasses import dataclass
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


@dataclass(frozen=True)
class PreviewCase:
    """Fern fixture output and the expected workflow status and GitHub output."""

    name: str
    fern_exit: int
    stdout: str
    expected_exit: int
    stderr: str = ""
    expected_output: str = ""
    expected_error: str | None = None


class FernDocsPreviewTests(unittest.TestCase):
    def test_preview_requires_success_and_a_url(self):
        workflow = (
            Path(__file__).resolve().parents[1]
            / "workflows/fern-docs-preview-comment.yml"
        ).read_text()
        step_marker = "      - name: Generate preview URL\n"
        self.assertIn(step_marker, workflow, "Fern workflow must declare Generate preview URL")
        step = workflow.split(step_marker, 1)[1].split("      - name:", 1)[0]
        run_marker = "        run: |\n"
        self.assertIn(run_marker, step, "Generate preview URL must declare a literal run block")
        script = textwrap.dedent(step.split(run_marker, 1)[1])
        preview_url = "https://preview.example.test"
        published = f"[docs]: Published docs to {preview_url}"
        cases = [
            # Fern can print a URL before reporting a doc/link error.
            PreviewCase(
                "failure with URL",
                fern_exit=23,
                stdout=published,
                stderr="invalid documentation link",
                expected_exit=23,
                expected_error="::error::Fern preview generation failed (exit 23).",
            ),
            PreviewCase(
                "success with URL",
                fern_exit=0,
                stdout=published,
                expected_exit=0,
                expected_output=f"preview_url={preview_url}\n",
            ),
            PreviewCase(
                "success without URL",
                fern_exit=0,
                stdout="[docs]: No preview published",
                expected_exit=1,
                expected_error="::error::Fern exited successfully without a preview URL.",
            ),
        ]
        for case in cases:
            with self.subTest(scenario=case.name), tempfile.TemporaryDirectory() as directory:
                fixture = Path(directory)
                fern_directory = fixture / "fern"
                fern_directory.mkdir()
                executable = fixture / "bin" / "fern"
                executable.parent.mkdir()
                executable.write_text(
                    "#!/bin/sh\n"
                    'printf "%s\\n" "$@" > "$FERN_ARGS"\n'
                    'printf "%s\\n" "$FERN_STDOUT"\n'
                    'printf "%s\\n" "$FERN_STDERR" >&2\n'
                    'exit "$FERN_EXIT_STATUS"\n'
                )
                executable.chmod(0o700)
                github_output = fixture / "github-output"
                github_output.touch()
                fern_arguments = fixture / "fern-args"
                result = subprocess.run(
                    ["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", script],
                    cwd=fern_directory,
                    env={
                        "PATH": f"{executable.parent}:{os.defpath}",
                        "TMPDIR": str(fixture),
                        "HEAD_REF": "feature/docs",
                        "GITHUB_OUTPUT": str(github_output),
                        "FERN_ARGS": str(fern_arguments),
                        "FERN_STDOUT": case.stdout,
                        "FERN_STDERR": case.stderr,
                        "FERN_EXIT_STATUS": str(case.fern_exit),
                    },
                    capture_output=True,
                    text=True,
                    timeout=15,
                )

                self.assertEqual(result.returncode, case.expected_exit, result.stdout + result.stderr)
                self.assertEqual(github_output.read_text(), case.expected_output)
                self.assertIn(case.stdout, result.stdout)
                if case.stderr:
                    self.assertIn(case.stderr, result.stdout)
                if case.expected_error:
                    self.assertIn(case.expected_error, result.stdout)
                else:
                    self.assertNotIn("::error::", result.stdout)
                self.assertEqual(
                    fern_arguments.read_text().splitlines(),
                    ["generate", "--docs", "--preview", "--force", "--id", "feature-docs"],
                )


if __name__ == "__main__":
    unittest.main()
