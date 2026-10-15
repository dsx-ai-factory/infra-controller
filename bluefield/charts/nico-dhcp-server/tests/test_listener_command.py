# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Execute the rendered DHCP listener command without starting the server."""

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
