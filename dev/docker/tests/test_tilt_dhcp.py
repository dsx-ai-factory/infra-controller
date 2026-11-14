# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Check Tilt development image contracts against rendered Helm configuration."""

import json
from pathlib import Path
import subprocess
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[3]


class TiltDhcpTest(unittest.TestCase):
    def test_local_network_is_fully_rendered_by_helm(self):
        values = yaml.safe_load((ROOT / "dev/deployment/stack/values.yaml").read_text())
        resources = []
        for component in ("nico-dhcp", "nico-machine-a-tron"):
            result = subprocess.run(
                ["helm", "template", component, str(ROOT / "helm/charts" / component),
                 "--namespace", "nico-system", "--values", "-"],
                input=yaml.safe_dump({"global": values["global"], **values[component]}),
                capture_output=True, text=True, timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            resources.extend(r for r in yaml.safe_load_all(result.stdout) if r)
        services = {r["metadata"]["name"]: r["spec"] for r in resources if r["kind"] == "Service"}
        configs = {r["metadata"]["name"]: r["data"] for r in resources if r["kind"] == "ConfigMap"}
        server = services["nico-dhcp"]
        relay = services["nico-machine-a-tron-mat-0-dhcp-relay"]
        mat = configs["nico-machine-a-tron-mat-0-config-files"]["mat.toml"]
        kea = json.loads(configs["nico-dhcp-config"]["kea_config.json"])["Dhcp4"]
        identifier = next(o["data"] for o in kea["option-data"] if o["name"] == "dhcp-server-identifier")
        self.assertEqual(identifier, server["clusterIP"])
        self.assertIn('server_address = "' + server["clusterIP"] + ':' + str(server["ports"][0]["port"]) + '"', mat)
        self.assertIn('advertise_address = "' + relay["clusterIP"] + '"', mat)
        self.assertIn('listen_address = "0.0.0.0:' + str(relay["ports"][0]["port"]) + '"', mat)
        self.assertNotRegex(mat, r"__(NICO_DHCP|MAT_DHCP)_")
        console = values["nico-ssh-console-rs"]["configFiles"]["config"]
        host = next(line.split('"')[1] for line in console.splitlines() if line.startswith('override_bmc_ssh_host'))
        service_name, namespace, suffix = host.split('.', 2)
        self.assertEqual((namespace, suffix), ('nico-system', 'svc.cluster.local'))
        self.assertEqual(services[service_name]['clusterIP'], 'None')


if __name__ == "__main__":
    unittest.main()
