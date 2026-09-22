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
    def test_listener_addresses_arguments_and_exec(self):
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
