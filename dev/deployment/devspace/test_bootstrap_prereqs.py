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
    def test_next_step_profile(self):
        source = SCRIPT.read_text()
        summary = source[source.index("print_summary() {"):source.index("\nmain() {")]
        for rest, profile in (("1", "full"), ("0", "core-only")):
            with self.subTest(rest=rest):
                result = subprocess.run(
                    ["bash", "-c", summary + "\nprint_summary"],
                    env={**os.environ, "INSTALL_REST_PREREQS": rest,
                         "REPO_ROOT": "/repo", "NAMESPACE": "isolated"},
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"devspace deploy -n isolated --profile {profile}", result.stdout)

    def test_ssh_host_key_bootstrap(self):
        for existing in ("0", "1"):
            with self.subTest(existing=existing), tempfile.TemporaryDirectory() as directory:
                result = subprocess.run(
                    ["bash", "-c", '''
kubectl() {
    case "$*" in
        'apply -f -')
            cat >/dev/null
            [[ ! -e "$TEST_DIRECTORY/applied" ]] || exit 91
            touch "$TEST_DIRECTORY/applied"
            ;;
        'get secret ssh-host-key -n isolated') [[ "$TEST_EXISTING" == 1 ]] ;;
        'create secret generic ssh-host-key '*|'label secret ssh-host-key '*|'annotate secret ssh-host-key '*)
            printf '%s\\n' "$*" >> "$TEST_DIRECTORY/calls" ;;
        *) return 99 ;;
    esac
}
ssh-keygen() { printf '%s\\n' "ssh-keygen $*" >> "$TEST_DIRECTORY/calls"; }
# The key generator is mocked; do not remove any real host key files.
rm() { :; }
helm() { return 99; }
export -f kubectl ssh-keygen rm helm
bash "$1"
''', "bash", str(SCRIPT)],
                    env={**os.environ, "LOCAL_DEV_INSTALL_CERT_MANAGER": "0",
                         "LOCAL_DEV_NAMESPACE": "isolated", "LOCAL_DEV_INSTALL_POSTGRES": "1",
                         "TEST_DIRECTORY": directory, "TEST_EXISTING": existing},
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 91, result.stdout + result.stderr)
                calls = Path(directory) / "calls"
                if existing == "1":
                    self.assertFalse(calls.exists())
                else:
                    self.assertIn("ssh-keygen -t ed25519 -N", calls.read_text())
                    self.assertIn("create secret generic ssh-host-key --namespace isolated", calls.read_text())

    def test_rendered_vault_listener_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "vault.yaml"
            result = subprocess.run(
                ["bash", "-c", '''
kubectl() {
    case "$*" in
        'apply -f -')
            printf '%s\\n' '---' >> "$BOOTSTRAP_TEST_MANIFEST"
            cat >> "$BOOTSTRAP_TEST_MANIFEST"
            ;;
        'rollout status statefulset/vault '*) exit 91 ;;
        'get secret ssh-host-key '*) return 0 ;;
        *) return 1 ;;
    esac
}
helm() { return 1; }
export -f kubectl helm
source "$1"
''', "bash", str(SCRIPT)],
                env={
                    **os.environ,
                    "LOCAL_DEV_INSTALL_CERT_MANAGER": "0",
                    "LOCAL_DEV_INSTALL_POSTGRES": "0",
                    "LOCAL_DEV_INSTALL_VAULT": "1",
                    "LOCAL_DEV_VAULT_TOKEN": "test-token",
                    "BOOTSTRAP_TEST_MANIFEST": str(manifest),
                },
                capture_output=True, text=True, timeout=15,
            )
            # Capture the real Vault emitter before rollout or credential setup.
            self.assertEqual(result.returncode, 91, result.stderr)
            resources = {
                (item["kind"], item["metadata"]["name"]): item
                for item in yaml.safe_load_all(manifest.read_text())
            }
            statefulset = resources[("StatefulSet", "vault")]
            container = statefulset["spec"]["template"]["spec"]["containers"][0]
            self.assertIn("-dev-listen-address=[::]:8200", container["args"])
            environment = {item["name"]: item["value"] for item in container["env"]}
            self.assertEqual(environment["VAULT_DEV_LISTEN_ADDRESS"], "[::]:8200")

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
                ntp = resources[("Service", "nico-ntp-client")]
                self.assertEqual(ntp["spec"]["selector"], {"app.kubernetes.io/name": "nico-ntp"})
                self.assertEqual(ntp["spec"]["ports"][0], {
                    "name": "ntp", "port": 123, "targetPort": 123, "protocol": "UDP"
                })


if __name__ == "__main__":
    unittest.main()
