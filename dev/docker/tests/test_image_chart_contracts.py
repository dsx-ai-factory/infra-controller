# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Check Tilt development image contracts against rendered Helm configuration."""

import json
import re
from pathlib import Path
import subprocess
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[3]


class TiltGatewayTest(unittest.TestCase):
    def test_gateway_image_installs_the_helm_entrypoint(self):
        chart = ROOT / "helm/charts/nico-machine-a-tron/charts/mat-k8s-controller"
        suite = yaml.safe_load((chart / "tests/gateway_test.yaml").read_text())
        values = {**suite["set"], "enabled": True, "gateway": {"enabled": True}}
        result = subprocess.run(
            ["helm", "template", "mat", str(chart), "--values", "-",
             "--show-only", "templates/deployment.yaml"],
            input=yaml.safe_dump(values), capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        containers = yaml.safe_load(result.stdout)["spec"]["template"]["spec"]["containers"]
        command = next(c["command"][0] for c in containers if c["name"] == "gateway")
        dockerfile = (ROOT / "dev/docker/Dockerfile.dev-services").read_text()
        stage = dockerfile.split(" AS mat-protocol-gateway\n", 1)[1].split("\nFROM ", 1)[0]
        destinations = re.findall(r"\bcp /cargo-target/\S+ (\S+)", stage)
        self.assertIn(command, destinations)


class TiltDhcpTest(unittest.TestCase):
    def test_tilt_values_render_the_image_dhcpv4_hook_path(self):
        tilt_values = yaml.safe_load((ROOT / "dev/deployment/stack/values.yaml").read_text())
        values = {"global": tilt_values["global"], **tilt_values["nico-dhcp"]}
        result = subprocess.run(
            ["helm", "template", "nico-dhcp", str(ROOT / "helm/charts/nico-dhcp"),
             "--namespace", "nico-system", "--values", "-",
             "--show-only", "templates/configmap.yaml"],
            input=yaml.safe_dump(values), capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        config = json.loads(yaml.safe_load(result.stdout)["data"]["kea_config.json"])
        self.assertEqual(config["Dhcp4"]["hooks-libraries"][0]["library"], "/usr/lib/kea/hooks/libdhcp.so")


if __name__ == "__main__":
    unittest.main()
