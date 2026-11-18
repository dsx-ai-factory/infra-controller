# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Execute the rendered DHCP listener command without starting the server."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import yaml


CHART = Path(__file__).resolve().parents[1]


class ListenerCommandTest(unittest.TestCase):
    """Verifies chart compatibility and listener execution without starting the DHCP server."""

    def test_legacy_values_keep_workload_enabled_and_can_enable_nad(self):
        """Verifies legacy values preserve workload and NAD defaults for safe `--reuse-values` upgrades."""
        # Copy the chart and remove only values introduced by the service-VPC milestone.
        with tempfile.TemporaryDirectory() as directory:
            chart = Path(directory) / "nico-dhcp-server"
            shutil.copytree(CHART, chart)
            values_path = chart / "values.yaml"
            values = yaml.safe_load(values_path.read_text())
            values["serviceDaemonSet"].pop("enabled")
            values.pop("serviceVpcNetworks")
            values_path.write_text(yaml.safe_dump(values, sort_keys=False))

            # Render with the unchanged legacy values that Helm can retain during an upgrade.
            rendered = subprocess.run(
                ["helm", "template", "dhcp-test", str(chart)],
                capture_output=True, text=True, timeout=30,
            )

            # Enable only the new switch to mirror a direct `--reuse-values` upgrade.
            enabled_network = subprocess.run(
                [
                    "helm", "template", "dhcp-test", str(chart),
                    "--set", "serviceVpcNetworks.ipv4.enabled=true",
                ],
                capture_output=True, text=True, timeout=30,
            )

        # The unchanged legacy release must retain both workload resources and no opt-in NAD.
        self.assertEqual(rendered.returncode, 0, rendered.stderr)
        resources = list(yaml.safe_load_all(rendered.stdout))
        self.assertCountEqual(
            [resource["kind"] for resource in resources],
            ["DaemonSet", "Service"],
        )

        # The complete enabled release adds only the fixed NAD and its node-local CNI daemon.
        self.assertEqual(enabled_network.returncode, 0, enabled_network.stderr)
        enabled_resources = list(yaml.safe_load_all(enabled_network.stdout))
        self.assertCountEqual(
            [
                (resource["kind"], resource["metadata"]["name"])
                for resource in enabled_resources
            ],
            [
                ("DaemonSet", "dhcp-test"),
                ("DaemonSet", "dhcp-test-dhcp-cni"),
                ("Service", "dhcp-test"),
                ("NetworkAttachmentDefinition", "nico-service-vpc-ipv4"),
            ],
        )

        # Keep the DHCP server Service from selecting the separate CNI-daemon pods.
        service = next(
            resource for resource in enabled_resources if resource["kind"] == "Service"
        )
        cni_daemon = next(
            resource
            for resource in enabled_resources
            if resource["metadata"]["name"] == "dhcp-test-dhcp-cni"
        )
        self.assertNotEqual(
            service["spec"]["selector"]["app.kubernetes.io/name"],
            cni_daemon["spec"]["template"]["metadata"]["labels"][
                "app.kubernetes.io/name"
            ],
        )

        rendered_nads = [
            resource
            for resource in enabled_resources
            if resource["kind"] == "NetworkAttachmentDefinition"
        ]
        self.assertEqual(len(rendered_nads), 1)
        nad = rendered_nads[0]
        self.assertEqual(nad["metadata"]["name"], "nico-service-vpc-ipv4")
        network_config = json.loads(nad["spec"]["config"])
        self.assertEqual(network_config["name"], "nico-service-vpc-ipv4")
        self.assertEqual(network_config["mtu"], 9000)

    def test_listener_addresses_and_exec(self):
        rendered = subprocess.run(
            ["helm", "template", "dhcp-test", str(CHART), "--show-only", "templates/daemonset.yaml"],
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(rendered.returncode, 0, rendered.stderr)
        pod = yaml.safe_load(rendered.stdout)["spec"]["template"]["spec"]
        container = pod["containers"][0]
        self.assertEqual(container["args"], ["/var/support/forge-dhcp/bin/forge-dhcp-server"])
        self.assertEqual(container["command"][:2], ["/busybox/sh", "-ec"])
        pod_ip = next(item for item in container["env"] if item["name"] == "POD_IP")
        self.assertEqual(pod_ip["valueFrom"]["fieldRef"]["fieldPath"], "status.podIP")

        with tempfile.TemporaryDirectory() as directory:
            recorder = Path(directory) / "record-argv"
            recorder.write_text(
                f"#!{sys.executable}\n"
                "import json, os, sys\n"
                "print(json.dumps({'args': sys.argv[1:], 'pid': os.getpid()}))\n"
            )
            recorder.chmod(0o755)
            for pod_ip, listen_host in [
                ("192.0.2.17", "192.0.2.17"),
                ("2001:db8::17", "[2001:db8::17]"),
            ]:
                with self.subTest(pod_ip=pod_ip):
                    command = ["busybox", "sh", *container["command"][1:], str(recorder)]
                    with subprocess.Popen(
                        command, env={**os.environ, "POD_IP": pod_ip},
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    ) as process:
                        stdout, stderr = process.communicate(timeout=10)
                        self.assertEqual(process.returncode, 0, stderr)
                        recorded = json.loads(stdout)
                        self.assertEqual(recorded["args"], [
                            f"--grpc-listen-addr={listen_host}:10079",
                            f"--metrics-listen-addr={listen_host}:10080",
                        ])
                        self.assertEqual(recorded["pid"], process.pid)


if __name__ == "__main__":
    unittest.main()
