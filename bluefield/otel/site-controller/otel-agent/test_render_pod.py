# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Check resolver answer preservation in the packaged OTEL Pod template."""

from pathlib import Path
import subprocess
import sys
import unittest

import yaml


DIRECTORY = Path(__file__).resolve().parent
TEMPLATE = DIRECTORY / "otel-agent.yaml.template"


class RenderPodTest(unittest.TestCase):
    def test_resolved_addresses_remain_separate_host_aliases(self):
        cases = (
            ("single IPv4 answer", ("192.0.2.1",)),
            ("multiple IPv6 answers", ("2001:db8::1", "2001:db8::2")),
        )
        for name, addresses in cases:
            with self.subTest(name=name):
                result = subprocess.run(
                    [sys.executable, "-B", str(DIRECTORY / "render_pod.py"),
                     str(TEMPLATE), "carbide-api.forge", "\n".join(addresses)],
                    check=True, capture_output=True, text=True, timeout=10,
                )
                pod = yaml.safe_load(result.stdout)
                expected = yaml.safe_load(TEMPLATE.read_text(encoding="utf-8"))
                expected["spec"]["hostAliases"] = [
                    {"ip": address, "hostnames": ["carbide-api.forge"]}
                    for address in addresses
                ]
                self.assertEqual(pod, expected)


if __name__ == "__main__":
    unittest.main()
