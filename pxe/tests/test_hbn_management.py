# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Exercise both embedded HBN patches and their generated eth0 configuration.

HBN_TEST_MANIFEST can point to the official HBN 3.2.3 manifest for the same
checks against its management function. The default fixture is authored here;
the proprietary manifest is not redistributed with the test.
"""

import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
FIXTURE = Path(__file__).parent / "fixtures/hbn_management.sh"
TEMPLATES = ["pxe/templates/user-data", "crates/api/files/bf.cfg"]


class HbnManagementTest(unittest.TestCase):
    def test_patched_startup_preserves_management_families(self):
        cases = [
            {
                "name": "dual stack with fe80 in a global address and a link-local gateway",
                "ipv4_addresses": "    inet 10.244.7.9/24 brd 10.244.7.255 scope global eth0\n",
                "ipv6_addresses": (
                    "    inet6 2001:db8:fe80::2/72 scope global dynamic mngtmpaddr noprefixroute\n"
                    "       valid_lft 3600sec preferred_lft 1800sec\n"
                    "    inet6 2001:db8:1::2/64 scope global\n"
                ),
                "ipv4_routes": "default via 10.244.7.1 dev eth0\n",
                "ipv6_routes": "default via fe80::1 dev eth0 proto ra\n",
                "addresses": ["10.244.7.9/24", "2001:db8:fe80::2/72"],
                "gateways": ["10.244.7.1", "fe80::1"],
                "masks": ["24", "72"],
            },
            {
                "name": "IPv6 only",
                "ipv4_addresses": "",
                "ipv6_addresses": "    inet6 fd00:1234::2/80 scope global\n",
                "ipv4_routes": "",
                "ipv6_routes": "default via fd00:1234::1 dev eth0\n",
                "addresses": ["fd00:1234::2/80"],
                "gateways": ["fd00:1234::1"],
                "masks": ["", "80"],
            },
            {
                "name": "IPv4 recovery without a global IPv6 address",
                "ipv4_addresses": "    inet 10.244.7.130/25 scope global eth0\n",
                "ipv6_addresses": "",
                "ipv4_routes": "default nhid 12 proto bgp\n",
                "ipv6_routes": "default via fe80::1 dev eth0 proto ra\n",
                "addresses": ["10.244.7.130/25"],
                "gateways": ["10.244.7.129"],
                "masks": ["25", ""],
            },
        ]
        for template in TEMPLATES:
            for case in cases:
                with self.subTest(template=template, case=case["name"]):
                    self.check_management_config(template, case)

    def check_management_config(self, template, case):
        source = (REPO_ROOT / template).read_text()
        embedded = re.search(
            r"  - path: /opt/(?:forge|dpf)/patch-hbn-manifest.py\n"
            r".*?    content: \|\n(.*?)\n  - path:",
            source, re.DOTALL,
        )
        self.assertIsNotNone(embedded, f"missing embedded patch in {template}")
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            manifest = directory / "doca_hbn.yaml"
            manifest.write_text(Path(os.environ.get("HBN_TEST_MANIFEST", FIXTURE)).read_text())
            patch = directory / "patch-hbn-manifest.py"
            patch.write_text(textwrap.dedent(embedded.group(1)))
            patched = subprocess.run(
                [sys.executable, "-B", str(patch), str(manifest)],
                capture_output=True, text=True, timeout=15,
            )
            self.assertEqual(patched.returncode, 0, patched.stdout + patched.stderr)
            function = re.search(
                r"^      function setup_eth0_in_mgmt_vrf\(\) \{\n.*?^      \}",
                manifest.read_text(), re.DOTALL | re.MULTILINE,
            )
            self.assertIsNotNone(function, "missing management function after patching")

            ip = directory / "ip"
            ip.write_text(
                '#!/bin/bash\n'
                'case "$*" in\n'
                '  "-4 addr show eth0") printf "%s" "$IPV4_ADDRESSES" ;;\n'
                '  "-6 addr show eth0 scope global") printf "%s" "$IPV6_ADDRESSES" ;;\n'
                '  "-4 route show default") printf "%s" "$IPV4_ROUTES" ;;\n'
                '  "-6 route show default") printf "%s" "$IPV6_ROUTES" ;;\n'
                '  *) exit 1 ;;\n'
                'esac\n'
            )
            ip.chmod(0o700)
            # Only run the management function. Keep its output under the test
            # directory and suppress host sysctls when using the real manifest.
            shell = (
                'chroot() { return 0; }\n'
                + textwrap.dedent(function.group(0)).replace(
                    "/host/var/lib/hbn", str(directory / "hbn"),
                )
                + '\nsetup_eth0_in_mgmt_vrf\n'
                + 'printf "%s\\n" "$MASKLEN_IPV4" "$MASKLEN_IPV6"\n'
            )
            executed = subprocess.run(
                ["bash", "-c", shell],
                env={
                    **os.environ,
                    "PATH": f"{directory}:{os.environ['PATH']}",
                    **{key.upper(): case[key] for key in (
                        "ipv4_addresses", "ipv6_addresses", "ipv4_routes", "ipv6_routes",
                    )},
                },
                capture_output=True, text=True, timeout=15,
            )
            self.assertEqual(executed.returncode, 0, executed.stdout + executed.stderr)
            config_path = directory / "hbn/etc/network/interfaces.d/mgmt.intf"
            self.assertTrue(config_path.is_file(), "management function did not write mgmt.intf")
            config = config_path.read_text()
            # The real manifest also writes the mgmt VRF stanza. Compare the
            # complete eth0 stanza, including the absence of any empty fields.
            self.assertIn("auto eth0\n", config, "management config is missing the eth0 stanza")
            eth0 = "auto eth0\n" + config.split("auto eth0\n", 1)[1]
            expected = ["auto eth0", "iface eth0 inet static"]
            expected += [f"    address {address}" for address in case["addresses"]]
            expected += [f"    gateway {gateway}" for gateway in case["gateways"]]
            expected += ["    vrf mgmt"]
            self.assertEqual(eth0, "\n".join(expected) + "\n")
            self.assertEqual(executed.stdout.splitlines(), case["masks"])


if __name__ == "__main__":
    unittest.main()
