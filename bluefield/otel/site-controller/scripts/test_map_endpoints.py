# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Check address/name pair preservation without changing the host's /etc/hosts."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().with_name("map_endpoints.sh")


class MapEndpointsTest(unittest.TestCase):
    def test_preserves_both_families_and_is_idempotent(self):
        addresses = ("192.0.2.1", "2001:db8::1")
        with tempfile.TemporaryDirectory() as directory:
            hosts = Path(directory) / "hosts"
            original = "127.0.0.1 localhost\n192.0.2.99 unrelated.example\n"
            hosts.write_text(original, encoding="utf-8")
            endpoints = Path(directory) / "endpoints.json"
            endpoints.write_text(json.dumps({
                "endpoints": [{"ip": ip, "fqdn": "carbide-api.forge"} for ip in addresses],
            }), encoding="utf-8")
            self.run_mapper(endpoints, hosts)
            expected = original + "\n" + "".join(
                f"{ip} carbide-api.forge\n" for ip in addresses
            )
            self.assertEqual(hosts.read_text(encoding="utf-8"), expected)
            self.run_mapper(endpoints, hosts)
            self.assertEqual(hosts.read_text(encoding="utf-8"), expected)

    def test_only_existing_aliases_suppress_mapping(self):
        cases = (
            ("real alias", "2001:db8::1 primary.example carbide-api.forge # existing alias\n", ""),
            ("comment only", "2001:db8::1 primary.example # carbide-api.forge\n",
             "\n2001:db8::1 carbide-api.forge\n"),
        )
        for name, original, addition in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                hosts = Path(directory) / "hosts"
                hosts.write_text(original, encoding="utf-8")
                endpoints = Path(directory) / "endpoints.json"
                endpoints.write_text(json.dumps({
                    "endpoints": [{"ip": "2001:db8::1", "fqdn": "carbide-api.forge"}],
                }), encoding="utf-8")
                self.run_mapper(endpoints, hosts)
                self.assertEqual(hosts.read_text(encoding="utf-8"), original + addition)

    def run_mapper(self, endpoints, hosts):
        # Redirect only the fixed output path; execute the packaged script's logic.
        script = SCRIPT.read_text(encoding="utf-8")
        self.assertEqual(script.count('HOSTS_FILE="/etc/hosts"'), 1)
        script = script.replace('HOSTS_FILE="/etc/hosts"', 'HOSTS_FILE="$MAPPER_TEST_HOSTS"')
        result = subprocess.run(
            ["bash", "-s", "--", str(endpoints)], input=script,
            env={**os.environ, "MAPPER_TEST_HOSTS": str(hosts)},
            capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
