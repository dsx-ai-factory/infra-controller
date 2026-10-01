# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("prepare-ubuntu-host-for-dev.sh")


class KeaAppArmorTest(unittest.TestCase):
    def test_development_policy_and_reload(self):
        # Exercise the policy writer without running privileged host preparation.
        source = SCRIPT.read_text()
        function = source[source.index("configure_kea_apparmor() {"):
                          source.index("\nconfigure_k3s_kubeconfig() {")]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profiles = root / "apparmor.d"
            profiles.mkdir()
            profile = profiles / "usr.sbin.kea-dhcp4"
            profile.touch()
            function = function.replace("/etc/apparmor.d", str(profiles))
            result = subprocess.run(
                ["bash", "-c", '''
set -euo pipefail
log() { :; }
apparmor_parser() { printf '%s\\n' "$*" >> "$RELOAD_LOG"; }
''' + function + "\nconfigure_kea_apparmor\nconfigure_kea_apparmor\n"],
                env={**os.environ, "REPO_DIR": directory,
                     "RELOAD_LOG": str(root / "reloads")},
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            policy = (profiles / "local/usr.sbin.kea-dhcp4").read_text().splitlines()
            for rule in (
                "/run/kea/* rwk,",
                "/usr/lib/kea/hooks/*.so mr,",
                "/run/secrets/spiffe.io/** r,",
                "/tmp/** rwk,",
                f"{directory}/target/debug/*.so mr,",
            ):
                self.assertEqual(policy.count(rule), 1, policy)
            self.assertEqual((root / "reloads").read_text().splitlines(),
                             [f"-r {profile}", f"-r {profile}"])
            self.assertFalse((profiles / "local/usr.sbin.kea-dhcp6").exists())


if __name__ == "__main__":
    unittest.main()
