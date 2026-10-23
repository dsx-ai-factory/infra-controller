# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Check the Tilt DHCP image contract through the deployed Helm configuration."""

import json
from pathlib import Path
import subprocess
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[3]


class TiltDhcpTest(unittest.TestCase):
    def test_tilt_values_render_the_shared_hook_for_both_dhcp_families(self):
        tilt_values = yaml.safe_load((ROOT / "dev/deployment/tilt/values.yaml").read_text())
        values = {"global": tilt_values["global"], **tilt_values["nico-dhcp"]}
        # IPv6 is opt-in; use its required stable server identifier for this render.
        values.setdefault("dhcp", {}).update(
            v6Enabled=True, v6ServerIdentifier="020000000001",
        )
        result = subprocess.run(
            ["helm", "template", "nico-dhcp", str(ROOT / "helm/charts/nico-dhcp"),
             "--namespace", tilt_values["tilt"]["localConfig"]["coreNamespace"],
             "--values", "-", "--show-only", "templates/configmap.yaml",
             "--show-only", "templates/configmap-v6.yaml"],
            input=yaml.safe_dump(values), capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        configs = {
            filename: json.loads(content)
            for resource in yaml.safe_load_all(result.stdout)
            for filename, content in resource["data"].items()
        }
        self.assertEqual(set(configs), {"kea_config.json", "kea_config6.json"})
        for filename, family in (("kea_config.json", "Dhcp4"), ("kea_config6.json", "Dhcp6")):
            with self.subTest(family=family):
                self.assertEqual(
                    configs[filename][family]["hooks-libraries"][0]["library"],
                    "/usr/lib/kea/hooks/libdhcp.so",
                )


if __name__ == "__main__":
    unittest.main()
