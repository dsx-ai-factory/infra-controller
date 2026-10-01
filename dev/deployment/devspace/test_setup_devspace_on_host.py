# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("setup-devspace-on-host.sh")


class SetupDevSpaceOnHostTest(unittest.TestCase):
    def test_profile_selection(self):
        source = SCRIPT.read_text()
        # Exercise the real defaults, parser and deployment function without host setup.
        parser = source[:source.index('\nif [[ "${EUID}" -ne 0 ]]; then')]
        deploy = source[source.index("deploy_stack() {"):
                        source.index("\ntemporal_namespace_is_healthy() {")]
        for args, profile in (([], "full"),
                              (["--profile", "full", "--profile", "dsx-exchange"], "dsx-exchange")):
            with self.subTest(args=args), tempfile.TemporaryDirectory(prefix="repo space ") as directory:
                bootstrap = Path(directory) / "dev/deployment/devspace/bootstrap-prereqs.sh"
                bootstrap.parent.mkdir(parents=True)
                bootstrap.write_text("#!/bin/sh\nexit 0\n")
                bootstrap.chmod(0o755)
                result = subprocess.run(
                    ["bash", "-c", parser + deploy + '''
run_as_user() { "$@"; }
cache_postgres_wait_image() { :; }
devspace() { printf '<%s>\\n' "$@"; }
export -f devspace
deploy_stack
''', "bash", "--repo-dir", directory, *args],
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(f"<deploy>\n<-n>\n<nico-system>\n<--profile>\n<{profile}>\n",
                              result.stdout)

    @unittest.skipIf(os.geteuid() == 0, "sudo forwarding is only used by non-root callers")
    def test_profile_forwarded_through_sudo(self):
        with tempfile.TemporaryDirectory() as directory:
            sudo = Path(directory) / "sudo"
            sudo.write_text("#!/bin/sh\nprintf '<%s>\\n' \"$@\"\n")
            sudo.chmod(0o755)
            result = subprocess.run(
                ["bash", str(SCRIPT), "--profile", "dsx-exchange"],
                env={**os.environ, "PATH": f"{directory}:{os.environ['PATH']}"},
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("<--profile>\n<dsx-exchange>\n", result.stdout)

    def test_missing_profile(self):
        for args in (["--profile"], ["--profile", ""], ["--profile", "--skip-deploy"]):
            with self.subTest(args=args):
                result = subprocess.run(["bash", str(SCRIPT), *args],
                                        capture_output=True, text=True, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("--profile requires a value", result.stderr)


if __name__ == "__main__":
    unittest.main()
