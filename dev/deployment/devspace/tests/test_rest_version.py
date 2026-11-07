# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from pathlib import Path
import subprocess
import tempfile
import unittest

import yaml


REPO_ROOT = Path(__file__).resolve().parents[4]


class RestVersionTests(unittest.TestCase):
    def test_rest_image_uses_fresh_git_version(self):
        config = yaml.safe_load((REPO_ROOT / "devspace.yaml").read_text())
        self.assertEqual(config["images"]["nico-rest-api"]["buildArgs"]["VERSION"],
                         "${LOCAL_DEV_REST_VERSION}")
        variable = config["vars"]["LOCAL_DEV_REST_VERSION"]
        self.assertEqual(variable["source"], "command")
        self.assertTrue(variable["noCache"])
        command = [variable["command"], *variable["args"]]

        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.run(
                    ["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                     "-c", "commit.gpgsign=false", *args],
                    cwd=directory, check=True, capture_output=True, text=True,
                ).stdout.strip()

            def version():
                return subprocess.run(command, cwd=directory, capture_output=True, text=True)

            git("init")
            tracked = Path(directory) / "tracked"
            tracked.write_text("initial\n")
            git("add", "tracked")
            git("commit", "-m", "Initial fixture")
            with self.subTest(state="no reachable release tag"):
                self.assertNotEqual(version().returncode, 0)

            # Preserve the actual release, even if it is too old for an SDK.
            git("tag", "v0.1.0")
            with self.subTest(state="release"):
                result = version()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), "v0.1.0")

            git("commit", "--allow-empty", "-m", "Prerelease fixture")
            git("tag", "v2.4.0-pr")
            git("commit", "--allow-empty", "-m", "Development fixture")
            with self.subTest(state="post prerelease"):
                self.assertRegex(version().stdout.strip(), r"^v2\.4\.0-pr-1-g[0-9a-f]+$")

            tracked.write_text("modified\n")
            with self.subTest(state="dirty"):
                self.assertRegex(version().stdout.strip(), r"^v2\.4\.0-pr-1-g[0-9a-f]+-dirty$")
