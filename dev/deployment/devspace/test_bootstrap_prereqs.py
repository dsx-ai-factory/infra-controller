# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import yaml


SCRIPT = Path(__file__).with_name("bootstrap-prereqs.sh")


class BootstrapPrereqsTest(unittest.TestCase):
    def test_postgres_host_in_connection_objects(self):
        # A trailing colon breaks YAML; brackets must stay text, not a YAML list.
        for host in ("2001:db8:1:2:3:4::", "[2001:db8::1]", "postgres.example.test"):
            with self.subTest(host=host), tempfile.TemporaryDirectory() as directory:
                manifest = Path(directory) / "connection-objects.yaml"
                result = subprocess.run(
                    ["bash", "-c", '''
kubectl() {
    [[ "$*" == 'apply -f -' ]] || return 1
    cat > "$BOOTSTRAP_TEST_MANIFEST"
    exit 91
}
helm() { return 1; }
export -f kubectl helm
source "$1"
''', "bash", str(SCRIPT)],
                    env={
                        **os.environ,
                        "LOCAL_DEV_INSTALL_CERT_MANAGER": "0",
                        "LOCAL_DEV_NAMESPACE": "nico-system",
                        "LOCAL_DEV_POSTGRES_HOST": host,
                        "LOCAL_DEV_POSTGRES_USER": "nico",
                        "LOCAL_DEV_POSTGRES_PASSWORD": "nico",
                        "LOCAL_DEV_POSTGRES_DB": "nico",
                        "LOCAL_DEV_VAULT_TOKEN": "test-token",
                        "BOOTSTRAP_TEST_MANIFEST": str(manifest),
                    },
                    capture_output=True, text=True, timeout=15,
                )
                # Stop after the real emitter's first apply, before any installation.
                self.assertEqual(result.returncode, 91, result.stderr)
                resources = {
                    (item["kind"], item["metadata"]["name"]): item
                    for item in yaml.safe_load_all(manifest.read_text())
                }
                secret = resources[("Secret", "nico-system.nico.nico-pg-cluster.credentials")]
                config = resources[("ConfigMap", "nico-system-nico-database-config")]
                self.assertEqual(secret["stringData"]["host"], host)
                self.assertEqual(config["data"]["DB_HOST"], host)


if __name__ == "__main__":
    unittest.main()
