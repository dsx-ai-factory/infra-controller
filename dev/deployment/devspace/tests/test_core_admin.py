# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "core-admin.sh"


class CoreAdminTest(unittest.TestCase):
    def test_no_ready_pod_reports_namespace(self):
        with tempfile.TemporaryDirectory() as directory:
            kubectl = Path(directory) / "kubectl"
            kubectl.write_text("""#!/bin/sh
if [ "$1" = get ]; then
  printf '%s\\n' '{"items": []}'
else
  printf 'unexpected kubectl call\\n' >&2
  exit 99
fi
""")
            kubectl.chmod(0o755)
            result = subprocess.run(
                ["bash", str(SCRIPT), "machine", "list"],
                env={**os.environ, "PATH": f"{directory}:{os.environ['PATH']}",
                     "LOCAL_DEV_NAMESPACE": "fixture"},
                capture_output=True, text=True, timeout=10,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("no Ready nico-api pod in namespace fixture", result.stderr)
            self.assertNotIn("unexpected kubectl call", result.stderr)

    def test_client_timeout_reaches_remote_cli_without_changing_arguments(self):
        for timeout in (None, "15"):
            with self.subTest(timeout=timeout), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                cli = root / "nico-admin-cli"
                cli.write_text("""#!/bin/sh
printf 'timeout=%s tls=%s\\n' "${FORGE_CLIENT_REQUEST_TIMEOUT_SECS:-}" "${DISABLE_TLS_ENFORCEMENT:-}"
printf 'arg=%s\\n' "$@"
""")
                cli.chmod(0o755)
                kubectl = root / "kubectl"
                kubectl.write_text("""#!/usr/bin/env python3
import json, os, subprocess, sys
args = sys.argv[1:]
if args[0] == "get":
    print(json.dumps({"items": [{"metadata": {"name": "ready"}, "status": {
        "conditions": [{"type": "Ready", "status": "True"}]}}]}))
elif "-i" not in args:
    print(json.dumps({"data": {"certificate": "fixture-cert", "private_key": "fixture-key"}}))
else:
    remote = args[args.index("--") + 1:]
    remote[2] = remote[2].replace("/opt/carbide/nico-admin-cli", os.environ["FIXTURE_CLI"])
    env = dict(os.environ)
    env.pop("FORGE_CLIENT_REQUEST_TIMEOUT_SECS", None)
    sys.exit(subprocess.run(remote, env=env).returncode)
""")
                kubectl.chmod(0o755)
                environment = {**os.environ, "PATH": f"{directory}:{os.environ['PATH']}",
                               "FIXTURE_CLI": str(cli), "DISABLE_TLS_ENFORCEMENT": "1"}
                environment.pop("FORGE_CLIENT_REQUEST_TIMEOUT_SECS", None)
                if timeout is not None:
                    environment["FORGE_CLIENT_REQUEST_TIMEOUT_SECS"] = timeout
                result = subprocess.run(
                    ["bash", str(SCRIPT), "scout-stream", "ping", "fixture-machine"],
                    env=environment, capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"timeout={timeout or ''} tls=\n", result.stdout)
                self.assertTrue(result.stdout.endswith(
                    "arg=scout-stream\narg=ping\narg=fixture-machine\n"), result.stdout)


if __name__ == "__main__":
    unittest.main()
