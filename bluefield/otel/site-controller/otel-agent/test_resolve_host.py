# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Exercise API lookup through Pod installation without changing the host."""

import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import tempfile
import unittest

import yaml


DIRECTORY = Path(__file__).resolve().parent


class ResolveHostTest(unittest.TestCase):
    def test_installer_resolves_before_installing_pod(self):
        cases = (
            ("dual stack", ["2001:db8::1", "192.0.2.1", "2001:db8::1"],
             ["2001:db8::1", "192.0.2.1"]),
            ("lookup failure", None, []),
        )
        for name, answers, expected in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                private = root / "certs" / "private"
                private.mkdir(parents=True)
                (private / "otel-key.pem").touch()
                state = root / "state"
                state.mkdir()
                (state / "otel-agent-build.hash").write_text("test-version", encoding="utf-8")
                (root / "otel-agent").touch()
                config = root / "pod.yaml"
                (root / "sitecustomize.py").write_text(
                    "import json, os, socket\n"
                    "from pathlib import Path\n"
                    "def resolve(host, port, family, kind):\n"
                    "    root = Path(os.environ['OTEL_TEST_ROOT'])\n"
                    "    (root / 'query.json').write_text(json.dumps([host, port, family, kind]))\n"
                    "    addresses = json.loads(os.environ['OTEL_TEST_ADDRESSES'])\n"
                    "    if addresses is None:\n"
                    "        raise socket.gaierror(socket.EAI_NONAME, 'test lookup failure')\n"
                    "    return [(socket.AF_INET6 if ':' in ip else socket.AF_INET,\n"
                    "             kind, socket.IPPROTO_TCP, '', (ip, 0)) for ip in addresses]\n"
                    "socket.getaddrinfo = resolve\n",
                    encoding="utf-8",
                )
                runtime = root / "runtime"
                runtime.write_text(
                    "#!/usr/bin/env bash\n"
                    "set -eu\n"
                    "printf '%s\\n' \"$*\" >> \"$OTEL_TEST_ROOT/runtime-calls\"\n"
                    "case \"${1:-}\" in\n"
                    "  image) echo test-version ;;\n"
                    "  ps)\n"
                    "    count=0\n"
                    "    [[ ! -f \"$OTEL_TEST_ROOT/polls\" ]] || read -r count < \"$OTEL_TEST_ROOT/polls\"\n"
                    "    count=$((count + 1))\n"
                    "    printf '%s\\n' \"$count\" > \"$OTEL_TEST_ROOT/polls\"\n"
                    "    if ((count <= 2)); then echo otel-agent; fi\n"
                    "    ;;\n"
                    "esac\n",
                    encoding="utf-8",
                )
                runtime.chmod(0o700)

                # Redirect fixed paths and container commands; retain the installer logic.
                script = (DIRECTORY / "otel-agent.sh").read_text(encoding="utf-8")
                replacements = {
                    "DOCKER=/usr/bin/docker": f"DOCKER={shlex.quote(str(runtime))}",
                    "CTR=/usr/bin/ctr": f"CTR={shlex.quote(str(runtime))}",
                    "CRICTL=/usr/bin/crictl": f"CRICTL={shlex.quote(str(runtime))}",
                    "STATE_DIR=/var/lib/otel-agent": f"STATE_DIR={shlex.quote(str(state))}",
                    "CERTS_DIR=/etc/otelcol-contrib/certs": f"CERTS_DIR={shlex.quote(str(private.parent))}",
                    "TEMPLATE=/usr/share/otelcol-contrib/docker/otel-agent/otel-agent.yaml.template":
                        f"TEMPLATE={shlex.quote(str(DIRECTORY / 'otel-agent.yaml.template'))}",
                    "OTEL_AGENT_CONFIG=/etc/kubelet.d/otel-agent.yaml":
                        f"OTEL_AGENT_CONFIG={shlex.quote(str(config))}",
                    "OTEL_AGENT=/usr/bin/otel-agent":
                        f"OTEL_AGENT={shlex.quote(str(root / 'otel-agent'))}",
                    "DOCKERFILE=/usr/share/otelcol-contrib/docker/otel-agent/Dockerfile":
                        f"DOCKERFILE={shlex.quote(str(DIRECTORY / 'Dockerfile'))}",
                }
                for original, replacement in replacements.items():
                    self.assertEqual(script.count(original), 1, original)
                    script = script.replace(original, replacement)
                self.assertEqual(script.count("/tmp/otel-agent-"), 3)
                script = script.replace("/tmp/otel-agent-", f"{shlex.quote(str(root))}/otel-agent-")

                result = subprocess.run(
                    ["bash", "-s"], input=script, capture_output=True, text=True, timeout=10,
                    env={**os.environ, "PYTHONPATH": str(root), "PYTHONDONTWRITEBYTECODE": "1",
                         "OTEL_TEST_ROOT": str(root), "OTEL_TEST_ADDRESSES": json.dumps(answers)},
                )
                self.assertEqual(
                    json.loads((root / "query.json").read_text(encoding="utf-8")),
                    ["carbide-api.forge", None, socket.AF_UNSPEC, socket.SOCK_STREAM],
                )
                if answers is None:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Failed to resolve carbide-api.forge", result.stderr)
                    self.assertFalse(config.exists())
                    self.assertFalse((root / "runtime-calls").exists())
                    continue

                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("otel-agent stopped unexpectedly.", result.stdout)
                pod = yaml.safe_load(config.read_text(encoding="utf-8"))
                self.assertEqual(pod["spec"]["hostAliases"], [
                    {"ip": address, "hostnames": ["carbide-api.forge"]}
                    for address in expected
                ])


if __name__ == "__main__":
    unittest.main()
