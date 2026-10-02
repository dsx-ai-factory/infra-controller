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
            openssl_config = root / "developer config" / "openssl-compat.cnf"
            function = function.replace("/etc/apparmor.d", str(profiles))
            result = subprocess.run(
                ["bash", "-c", '''
set -euo pipefail
log() { :; }
apparmor_parser() { printf '%s\\n' "$*" >> "$RELOAD_LOG"; }
''' + function + '''
configure_kea_apparmor
test ! -e "$POLICY_DIRECTORY/local/usr.sbin.kea-dhcp6"
touch "$POLICY_DIRECTORY/usr.sbin.kea-dhcp6"
configure_kea_apparmor
'''],
                env={**os.environ, "REPO_DIR": directory,
                     "OPENSSL_COMPAT_CONFIG": str(openssl_config),
                     "POLICY_DIRECTORY": str(profiles),
                     "RELOAD_LOG": str(root / "reloads")},
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            for daemon in ("kea-dhcp4", "kea-dhcp6"):
                policy = (profiles / f"local/usr.sbin.{daemon}").read_text().splitlines()
                for rule in (
                    "/run/kea/* rwk,",
                    "/usr/lib/kea/hooks/*.so mr,",
                    "/run/secrets/spiffe.io/** r,",
                    "/tmp/** rwk,",
                    f'"{openssl_config}" r,',
                    f"{directory}/target/debug/*.so mr,",
                ):
                    self.assertEqual(policy.count(rule), 1, policy)
            self.assertEqual((root / "reloads").read_text().splitlines(),
                             [f"-r {profile}", f"-r {profile}",
                              f"-r {profiles / 'usr.sbin.kea-dhcp6'}"])


if __name__ == "__main__":
    unittest.main()
