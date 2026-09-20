# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class LocalCoreAddressTest(unittest.TestCase):
    def test_configure_local_core_site_agent(self):
        cases = [
            ("IPv6", "2001:db8::10", "", "[2001:db8::10]:1079"),
            ("bracketed IPv6", "[2001:db8::10]", "", "[2001:db8::10]:1079"),
            ("hostname fallback", "nico.example.test", "", "nico.example.test:1079"),
            ("preferred IPv4", "nico.example.test", "192.0.2.10", "192.0.2.10:1079"),
        ]
        for name, host, resolved_ipv4, expected in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                fixture = Path(directory)
                for certificate in ("ca.crt", "client.crt", "client.key"):
                    (fixture / certificate).touch()
                docker = fixture / "docker"
                docker.write_text(
                    "#!/bin/sh\n"
                    'case "$1" in\n'
                    "  ps) echo control-plane ;;\n"
                    '  exec) [ -z "$RESOLVED_IPV4" ] || printf "%s STREAM\\n" "$RESOLVED_IPV4" ;;\n'
                    "  *) exit 1 ;;\n"
                    "esac\n"
                )
                kubectl = fixture / "kubectl"
                kubectl.write_text(
                    "#!/bin/sh\n"
                    "while [ $# -gt 0 ]; do\n"
                    '  if [ "$1" = "-p" ]; then\n'
                    '    printf "%s" "$2" > "$PATCH_FILE"\n'
                    "    exit 0\n"
                    "  fi\n"
                    "  shift\n"
                    "done\n"
                )
                docker.chmod(0o700)
                kubectl.chmod(0o700)
                patch_file = fixture / "patch.json"
                result = subprocess.run(
                    [
                        "make", "--no-print-directory", "-C",
                        str(Path(__file__).resolve().parents[1]),
                        "configure-local-core-site-agent",
                        f"LOCAL_CORE_HOST={host}",
                        "LOCAL_CORE_PORT=1079",
                        f"LOCAL_CORE_CERTS_DIR={fixture}",
                    ],
                    env={
                        **os.environ,
                        "PATH": f"{fixture}:{os.environ['PATH']}",
                        "RESOLVED_IPV4": resolved_ipv4,
                        "PATCH_FILE": str(patch_file),
                    },
                    capture_output=True, text=True, timeout=15,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                data = json.loads(patch_file.read_text())["data"]
                self.assertEqual(data["CORE_GRPC_ADDRESS"], expected)
                self.assertEqual(data["CARBIDE_ADDRESS"], expected)
                self.assertIn(f"CORE_GRPC_ADDRESS={expected},", result.stdout)


if __name__ == "__main__":
    unittest.main()
