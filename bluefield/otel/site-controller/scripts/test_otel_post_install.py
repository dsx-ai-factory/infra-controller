# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Check OTEL hostname selection without changing the host's name or files."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().with_name("otel_post_install.sh")


class OtelPostInstallTest(unittest.TestCase):
    def test_hostname_selection(self):
        ipv4 = {"family": "inet", "scope": "global", "local": "192.0.2.7"}
        ipv6 = {"family": "inet6", "scope": "global", "local": "2001:db8::9"}
        unsuitable = [
            {"family": "inet6", "scope": "link", "local": "fe80::1"},
            *(
                {"family": "inet6", "scope": "global", "local": f"2001:db8::{i}", flag: True}
                for i, flag in enumerate(("temporary", "tentative", "dadfailed", "deprecated"), 1)
            ),
        ]
        cases = (
            ("IPv4 unchanged", [ipv4], "example.test", "192-0-2-7.site.example.test", None),
            ("dual stack prefers IPv4", [ipv6, ipv4], "example.test", "192-0-2-7.site.example.test", None),
            ("lowest usable IPv4", [
                {"family": "inet", "scope": "link", "local": "169.254.0.1"},
                {"family": "inet", "scope": "global", "local": "192.0.2.1", "deprecated": True},
                {"family": "inet", "scope": "global", "local": "192.0.2.10"},
                ipv4,
            ], "example.test", "192-0-2-7.site.example.test", None),
            ("IPv6 stable selection", [
                *unsuitable,
                {"family": "inet6", "scope": "global", "local": "2001:db8::10"},
                ipv6,
            ], "example.test", "2001-0db8-0000-0000-0000-0000-0000-0009.site.example.test", None),
            ("64-character hostname", [ipv6], "longer.example.test",
             "2001-0db8-0000-0000-0000-0000-0000-0009.site.longer.example.test", None),
            ("65-character hostname", [ipv6], "longest.example.test", None,
             "hostname exceeds Linux's 64-character limit"),
            ("no addresses", [], "example.test", None, "no usable address found on oob_net0"),
            ("no eligible IPv6", unsuitable, "example.test", None, "no usable address found on oob_net0"),
        )
        for name, addresses, domain, expected, expected_error in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                site = root / "site-dpu.json"
                site.write_text(json.dumps({
                    "site": "site", "domain": domain, "endpoints": [],
                }), encoding="utf-8")
                address_file = root / "addresses.json"
                address_file.write_text(json.dumps([{"addr_info": addresses}]), encoding="utf-8")
                hostname = root / "hostname"
                hostname.write_text("old-hostname\n", encoding="utf-8")
                hosts = root / "hosts"
                original_hosts = "127.0.0.1 localhost.localdomain localhost\n"
                hosts.write_text(original_hosts, encoding="utf-8")
                calls = root / "calls"
                calls.write_text("", encoding="utf-8")
                bin_dir = root / "bin"
                bin_dir.mkdir()
                stub = """#!/usr/bin/env bash
set -euo pipefail
case "${0##*/}" in
    sudo)
        [[ "$1" == "ip" ]]
        shift
        exec "$POST_INSTALL_TEST_BIN/ip" "$@"
        ;;
    ip)
        [[ "$*" == "-json addr show oob_net0" ]]
        cat "$POST_INSTALL_TEST_ADDRESSES"
        ;;
    hostname)
        cat "$POST_INSTALL_TEST_HOSTNAME"
        ;;
    hostnamectl)
        [[ "$1" == "set-hostname" ]]
        printf '%s\\n' "$2" > "$POST_INSTALL_TEST_HOSTNAME"
        printf 'hostnamectl %s\\n' "$2" >> "$POST_INSTALL_TEST_CALLS"
        ;;
    localhost_alias.sh)
        printf '%s\\n' "$1" >> "$POST_INSTALL_TEST_HOSTS"
        printf 'alias %s\\n' "$1" >> "$POST_INSTALL_TEST_CALLS"
        ;;
    map_endpoints.sh)
        [[ "$1" == "$POST_INSTALL_TEST_SITE" ]]
        printf 'map\\n' >> "$POST_INSTALL_TEST_CALLS"
        ;;
    *) exit 99 ;;
esac
"""
                for command in ("sudo", "ip", "hostname", "hostnamectl", "localhost_alias.sh", "map_endpoints.sh"):
                    path = bin_dir / command
                    path.write_text(stub, encoding="utf-8")
                    path.chmod(0o755)

                # Redirect fixed paths, but execute the packaged script's selection and guards.
                script = SCRIPT.read_text(encoding="utf-8")
                replacements = (
                    ("SITE_FILE=/etc/site-dpu.json", 'SITE_FILE="$POST_INSTALL_TEST_SITE"'),
                    ("SCRIPT_DIR=/usr/local/sbin", 'SCRIPT_DIR="$POST_INSTALL_TEST_BIN"'),
                    ('"$SCRIPT_DIR"/map_endpoints.sh /etc/site-dpu.json',
                     '"$SCRIPT_DIR"/map_endpoints.sh "$SITE_FILE"'),
                )
                for source, replacement in replacements:
                    self.assertEqual(script.count(source), 1)
                    script = script.replace(source, replacement)
                env = {
                    **os.environ,
                    "PATH": f"{bin_dir}:{os.environ['PATH']}",
                    "POST_INSTALL_TEST_BIN": str(bin_dir),
                    "POST_INSTALL_TEST_SITE": str(site),
                    "POST_INSTALL_TEST_ADDRESSES": str(address_file),
                    "POST_INSTALL_TEST_HOSTNAME": str(hostname),
                    "POST_INSTALL_TEST_HOSTS": str(hosts),
                    "POST_INSTALL_TEST_CALLS": str(calls),
                }
                result = subprocess.run(
                    ["bash", "-s"], input=script, env=env,
                    capture_output=True, text=True, timeout=10,
                )
                if expected is None:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(expected_error, result.stderr)
                    self.assertEqual(hostname.read_text(encoding="utf-8"), "old-hostname\n")
                    self.assertEqual(hosts.read_text(encoding="utf-8"), original_hosts)
                    self.assertEqual(calls.read_text(encoding="utf-8"), "")
                    continue

                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(hostname.read_text(encoding="utf-8"), expected + "\n")
                self.assertEqual(hosts.read_text(encoding="utf-8"), original_hosts + expected + "\n")
                expected_calls = f"hostnamectl {expected}\nalias {expected}\nmap\n"
                self.assertEqual(calls.read_text(encoding="utf-8"), expected_calls)
                label = expected.split(".", 1)[0]
                self.assertLessEqual(len(label), 63)
                self.assertRegex(label, r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$")

                address_file.write_text(json.dumps([{"addr_info": list(reversed(addresses))}]), encoding="utf-8")
                result = subprocess.run(
                    ["bash", "-s"], input=script, env=env,
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(hostname.read_text(encoding="utf-8"), expected + "\n")
                self.assertEqual(hosts.read_text(encoding="utf-8"), original_hosts + expected + "\n")
                self.assertEqual(calls.read_text(encoding="utf-8"), expected_calls + "map\n")


if __name__ == "__main__":
    unittest.main()
