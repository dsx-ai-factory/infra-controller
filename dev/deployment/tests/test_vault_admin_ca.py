# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "stack/vault-admin-ca.py"
CA = SCRIPT.parents[2] / "certs/localhost/ca.crt"


class VaultAdminCaTest(unittest.TestCase):
    def test_export_reuse_and_failed_refresh(self):
        with tempfile.TemporaryDirectory(prefix="vault ca ") as directory:
            root = Path(directory)
            command = root / "kubectl"
            command.write_text(f"#!{sys.executable}\n" + '''
import os, sys
from pathlib import Path
assert sys.argv[1:] == ["--context", "kind-test", "--request-timeout=30s", "-n", "core-test",
                       "exec", "statefulset/nico-vault", "--", "env", "VAULT_ADDR=http://127.0.0.1:8200",
                       "vault", "read", "-field=certificate", "certs/cert/ca"]
mode = os.environ["TEST_MODE"]
if mode == "unavailable":
    sys.exit(42)
print(Path(os.environ["TEST_CA"]).read_text() if mode == "valid" else mode)
''')
            command.chmod(0o755)
            output = root / "new directory/ca.json"
            args = [sys.executable, "-B", str(SCRIPT), "--context", "kind-test",
                    "--namespace", "core-test", "--pki-mount", "certs", "--output", str(output)]
            env = {**os.environ, "PATH": f"{root}:{os.environ['PATH']}", "TEST_CA": str(CA)}
            for mode in ("valid", "valid", "unavailable", "-----BEGIN PRIVATE KEY-----",
                         "-----BEGIN CERTIFICATE-----\ninvalid\n-----END CERTIFICATE-----"):
                with self.subTest(mode=mode):
                    before = output.stat().st_mtime_ns if output.exists() else None
                    result = subprocess.run(args, env={**env, "TEST_MODE": mode},
                                            capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 0 if mode == "valid" else 1, result.stderr)
                    self.assertEqual(json.loads(output.read_text())["siteConfig"]["adminRootCertPem"].strip(),
                                     CA.read_text().strip())
                    if before is not None:
                        self.assertEqual(output.stat().st_mtime_ns, before)

    def test_missing_openssl_is_identified_without_creating_output(self):
        with tempfile.TemporaryDirectory(prefix="vault missing dependency ") as directory:
            root = Path(directory)
            command = root / "kubectl"
            command.write_text(f"#!{sys.executable}\nfrom pathlib import Path\nprint(Path({str(CA)!r}).read_text())\n")
            command.chmod(0o755)
            output = root / "ca.json"
            result = subprocess.run([sys.executable, "-B", str(SCRIPT), "--context", "kind-test",
                                     "--namespace", "core", "--pki-mount", "certs", "--output", str(output)],
                                    env={**os.environ, "PATH": str(root)},
                                    capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 1)
            self.assertIn("No such file or directory: 'openssl'", result.stderr)
            self.assertIn("output was not changed", result.stderr)
            self.assertNotIn("BEGIN CERTIFICATE", result.stderr)
            self.assertFalse(output.exists())

    def test_production_context_rejected_before_export(self):
        result = subprocess.run([sys.executable, "-B", str(SCRIPT), "--context", "production",
                                 "--namespace", "core", "--pki-mount", "certs", "--output", "/unused"],
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 2)
        self.assertIn("kind- context", result.stderr)


if __name__ == "__main__":
    unittest.main()
