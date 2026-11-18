# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Execute the rendered FMDS listener command without starting the server."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import yaml


CHART = Path(__file__).resolve().parents[1]


class ListenerCommandTest(unittest.TestCase):
    def test_ipv6_metadata_requires_an_ipv6_listener(self):
        """Reject an opt-in that would leave FMDS with only IPv4 listeners."""
        cases = [
            # Omission selects the binary's IPv4 listener default.
            {},
            # A compatibility listener cannot satisfy the requirement if it is also IPv4.
            {"restAddress": "0.0.0.0:80", "compatibilityRestAddress": "0.0.0.0:7777"},
        ]
        for listeners in cases:
            with self.subTest(listeners=listeners):
                rendered = subprocess.run(
                    ["helm", "template", "fmds-test", str(CHART), "--values", "-"],
                    input=yaml.safe_dump({"interfaceCidrV6": "fd00:76b9::254/127", **listeners}),
                    capture_output=True, text=True, timeout=30,
                )
                self.assertNotEqual(rendered.returncode, 0)
                self.assertIn("interfaceCidrV6 requires an IPv6", rendered.stderr)

    def test_metadata_settings_reject_invalid_schema_values(self):
        """Reject values the chart cannot pass to its metadata configuration consumer."""
        cases = [
            # The fixed peer needs the exact /127, not an arbitrary connected prefix.
            (CHART, {"interfaceCidrV6": "fd00:76b9::254/64"}, "interfaceCidrV6"),
            # The agent config serializes a list; a scalar must not render as valid TOML.
            (CHART.parent / "nico-dpu-agent",
             {"fmds": {"networkingAddresses": "fd00:76b9::254/127"}},
             "networkingAddresses"),
        ]
        for chart, values, field in cases:
            with self.subTest(chart=chart.name, field=field):
                rendered = subprocess.run(
                    ["helm", "template", "fmds-test", str(chart), "--values", "-"],
                    input=yaml.safe_dump(values),
                    capture_output=True, text=True, timeout=30,
                )
                self.assertNotEqual(rendered.returncode, 0)
                self.assertIn(field, rendered.stderr)

    def test_listener_addresses_arguments_and_exec(self):
        """Preserve exact listener arguments through exec so chart values reach FMDS unchanged."""
        cases = [
            ("IPv4 certificate mode", "192.0.2.17", {}, [
                "--grpc-address=192.0.2.17:50052",
                "--root-ca=/opt/forge/forge_root.pem",
                "--client-cert=/opt/forge/machine_cert.pem",
                "--client-key=/opt/forge/machine_cert.key",
            ]),
            ("IPv6 token mode", "2001:db8::17", {"useNodeTokens": True}, [
                "--grpc-address=[2001:db8::17]:50052",
                "--root-ca=/opt/forge/pub/forge_root.pem",
                "--node-token-socket=/opt/forge/run/agent.sock",
            ]),
            ("configured arguments stay literal", "2001:db8::17", {
                "restAddress": "[2001:db8::20]:8080",
                "compatibilityRestAddress": "127.0.0.1:7777",
                "certsDir": "/custom/cert dir",
                "rootCaFile": "root-$CA.pem",
            }, [
                "--grpc-address=[2001:db8::17]:50052",
                "--rest-address=[2001:db8::20]:8080",
                "--compatibility-rest-address=127.0.0.1:7777",
                "--root-ca=/custom/cert dir/root-$CA.pem",
                "--client-cert=/custom/cert dir/machine_cert.pem",
                "--client-key=/custom/cert dir/machine_cert.key",
            ]),
            # IPv6 metadata requires both the assigned address and an IPv6-capable
            # listener; neither option should alter the management bind or credentials.
            ("IPv6 metadata opt-in", "192.0.2.17", {
                "restAddress": "[::]:80",
                "interfaceCidrV6": "fd00:76b9::254/127",
            }, [
                "--grpc-address=192.0.2.17:50052",
                "--rest-address=[::]:80",
                "--interface-cidr-v6=fd00:76b9::254/127",
                "--root-ca=/opt/forge/forge_root.pem",
                "--client-cert=/opt/forge/machine_cert.pem",
                "--client-key=/opt/forge/machine_cert.key",
            ]),
            # An exact IPv6 compatibility listener retains the default IPv4 listener.
            # Expanded spelling is valid and must not be rejected by the chart guard.
            ("IPv6 compatibility listener", "192.0.2.17", {
                "interfaceCidrV6": "fd00:76b9::254/127",
                "compatibilityRestAddress": "[fd00:76b9:0:0:0:0:0:254]:7777",
            }, [
                "--grpc-address=192.0.2.17:50052",
                "--compatibility-rest-address=[fd00:76b9:0:0:0:0:0:254]:7777",
                "--interface-cidr-v6=fd00:76b9::254/127",
                "--root-ca=/opt/forge/forge_root.pem",
                "--client-cert=/opt/forge/machine_cert.pem",
                "--client-key=/opt/forge/machine_cert.key",
            ]),
        ]
        with tempfile.TemporaryDirectory() as directory:
            recorder = Path(directory) / "record-argv"
            recorder.write_text(
                f"#!{sys.executable}\n"
                "import json, os, sys\n"
                "print(json.dumps({'args': sys.argv[1:], 'pid': os.getpid()}))\n"
            )
            recorder.chmod(0o755)
            for name, pod_ip, values, expected in cases:
                with self.subTest(name=name):
                    rendered = subprocess.run(
                        ["helm", "template", "fmds-test", str(CHART),
                         "--show-only", "templates/daemonset.yaml", "--values", "-"],
                        input=yaml.safe_dump(values),
                        capture_output=True, text=True, timeout=30,
                    )
                    self.assertEqual(rendered.returncode, 0, rendered.stderr)
                    pod = yaml.safe_load(rendered.stdout)["spec"]["template"]["spec"]
                    container = pod["containers"][0]
                    self.assertEqual(container["args"][0], "/usr/bin/carbide-fmds")
                    self.assertEqual(container["command"][:2], ["/busybox/sh", "-ec"])
                    pod_ip_env = next(item for item in container["env"] if item["name"] == "POD_IP")
                    self.assertEqual(pod_ip_env["valueFrom"]["fieldRef"]["fieldPath"], "status.podIP")
                    command = ["busybox", "sh", *container["command"][1:],
                               str(recorder), *container["args"][1:]]
                    with subprocess.Popen(
                        command, env={**os.environ, "POD_IP": pod_ip},
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    ) as process:
                        stdout, stderr = process.communicate(timeout=10)
                        self.assertEqual(process.returncode, 0, stderr)
                        recorded = json.loads(stdout)
                        self.assertEqual(recorded["args"], expected)
                        self.assertEqual(recorded["pid"], process.pid)


if __name__ == "__main__":
    unittest.main()
