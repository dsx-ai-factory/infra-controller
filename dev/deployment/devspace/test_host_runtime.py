# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT_DIR = Path(__file__).resolve().parent


class NativePreparationTests(unittest.TestCase):
    def test_kind_runtime_configuration_is_idempotent(self):
        script = (SCRIPT_DIR / "setup-devspace-on-host.sh").read_text()
        function = script.split("configure_kind_node_tls() {", 1)[1].split("\n}\n", 1)[0]
        with tempfile.TemporaryDirectory(prefix="nico-kind-tls-test-") as temporary:
            root = Path(temporary)
            config, calls = root / "config", root / "calls"
            command = '''set -eu
log() { :; }
run_as_user() {
  printf '%s\\n' "$*" >> "$CALL_LOG"
  case "$*" in
    'docker exec fixture-control-plane cat '*) cat "$CONFIG" ;;
    'docker exec -i fixture-control-plane tee '*) cat > "$CONFIG" ;;
  esac
}
configure_kind_node_tls() {''' + function + '''
}
configure_kind_node_tls
configure_kind_node_tls
'''
            subprocess.run(["bash", "-c", command], check=True, capture_output=True,
                           text=True, env=dict(os.environ, CLUSTER_NAME="fixture",
                                               CONFIG=str(config), CALL_LOG=str(calls)))
            self.assertEqual(calls.read_text().count("systemctl restart containerd"), 1)

    def test_docker_configuration_only_restarts_changed_running_services(self):
        script = (SCRIPT_DIR / "setup-devspace-on-host.sh").read_text()
        function = script.split("configure_docker() {", 1)[1].split("\n}\n", 1)[0]
        config = '[Service]\nEnvironment="GODEBUG=tlsmlkem=0"\n'
        for active, existing, restarted in (
                ("0", None, []),
                ("1", None, ["containerd.service", "docker.service"]),
                ("1", config, [])):
            with self.subTest(active=active, existing=existing), \
                    tempfile.TemporaryDirectory(prefix="nico-docker-test-") as temporary:
                root = Path(temporary)
                for service in ("containerd", "docker"):
                    directory = root / f"{service}.service.d"
                    directory.mkdir()
                    if existing is not None:
                        (directory / "10-tls-compat.conf").write_text(existing)
                calls = root / "calls"
                command = '''set -eu
log() { :; }
die() { exit 1; }
usermod() { :; }
docker() { :; }
run_as_user() { "$@"; }
systemctl() {
  printf '%s\\n' "$*" >> "$CALL_LOG"
  if [ "$1" = is-active ]; then [ "$ACTIVE" = 1 ]; fi
}
configure_docker() {''' + function.replace("/etc/systemd/system", str(root)) + '''
}
configure_docker
'''
                subprocess.run(["bash", "-c", command], check=True, capture_output=True,
                               text=True, env=dict(os.environ, DEV_USER="fixture",
                                                   ACTIVE=active, CALL_LOG=str(calls)))
                actions = calls.read_text().splitlines()
                expected = ["restart " + " ".join(restarted)] if restarted else []
                self.assertEqual([line for line in actions if line.startswith("restart ")], expected)
                self.assertEqual("daemon-reload" in actions, existing != config)
                for service in ("containerd", "docker"):
                    self.assertEqual((root / f"{service}.service.d/10-tls-compat.conf").read_text(), config)

    def test_postgres_connection_budget_for_new_and_existing_containers(self):
        script = (SCRIPT_DIR / "prepare-ubuntu-host-for-dev.sh").read_text()
        function = script.split("start_core_postgres() {", 1)[1].split("\n}\n", 1)[0]
        with tempfile.TemporaryDirectory(prefix="nico-postgres-test-") as temporary:
            root = Path(temporary)
            state = root / "connections"
            calls = root / "calls"
            command = '''set -eu
log() { :; }
die() { printf '%s\\n' "$*" >&2; exit 1; }
wait_for_core_postgres() { :; }
run_as_user() {
  printf '%s\\n' "$*" >> "$CALL_LOG"
  case "$*" in
    'docker container inspect fixture') [ "$EXISTS" = 1 ] ;;
    *PortBindings*) printf '%s\\n' '{"5432/tcp":[{"HostIp":"127.0.0.1","HostPort":"5432"}]}' ;;
    'docker run '*) printf '1000\\n' > "$CONNECTIONS" ;;
    *'SHOW max_connections'*) cat "$CONNECTIONS" ;;
    *'ALTER SYSTEM SET max_connections'*) printf '1000\\n' > "$CONNECTIONS" ;;
  esac
}
start_core_postgres() {''' + function + '\n}\nstart_core_postgres\n'
            for exists, initial, restarts in (("0", 0, 0), ("1", 100, 1), ("1", 1000, 0)):
                with self.subTest(exists=exists, initial=initial):
                    state.write_text(str(initial))
                    calls.write_text("")
                    subprocess.run(["bash", "-c", command], check=True, text=True,
                                   capture_output=True, env=dict(os.environ,
                                   CONNECTIONS=str(state), CALL_LOG=str(calls),
                                   EXISTS=exists, CORE_POSTGRES_IMAGE="postgres:fixture",
                                   SKIP_CORE_POSTGRES="0", CORE_POSTGRES_CONTAINER="fixture"))
                    self.assertEqual(int(state.read_text()), 1000)
                    actions = calls.read_text()
                    self.assertEqual(actions.count("docker restart fixture"), restarts)
                    self.assertNotIn("docker rm", actions)
                    if exists == "1":
                        self.assertNotIn("docker run", actions)
                    else:
                        self.assertIn("-c max_connections=1000", actions)


class PurgeTests(unittest.TestCase):
    def test_reset_uses_active_docker_root_or_explicit_override(self):
        script = (SCRIPT_DIR / "reset-devspace-on-host.sh").read_text()
        function = script.split("resolve_docker_root() {", 1)[1].split("\n}\n", 1)[0]
        for explicit, active, expected in (
                ("0", "/custom/docker", "/custom/docker"),
                ("0", "", "/var/lib/docker"),
                ("1", "/custom/docker", "/explicit/docker")):
            with self.subTest(explicit=explicit, active=active):
                command = ('set -eu\nlog() { :; }\ndocker() { printf "%s" "$ACTIVE_ROOT"; }\n'
                           'resolve_docker_root() {' + function + '\n}\n'
                           'resolve_docker_root\nprintf "%s" "$DOCKER_ROOT"')
                result = subprocess.run(["bash", "-c", command], check=True, text=True,
                                        capture_output=True, env=dict(os.environ,
                                        DOCKER_ROOT_EXPLICIT=explicit, ACTIVE_ROOT=active,
                                        DOCKER_ROOT="/explicit/docker"))
                self.assertEqual(result.stdout, expected)

    def test_purge_preserves_family_and_fails_before_delete_without_node_config(self):
        with tempfile.TemporaryDirectory(prefix="nico-kind-test-") as temporary:
            root = Path(temporary)
            shutil.copyfile(SCRIPT_DIR / "reset-kind-cluster.sh", root / "reset.sh")
            (root / "bootstrap-prereqs.sh").write_text("#!/bin/sh\nexit 0\n")
            (root / "bootstrap-prereqs.sh").chmod(0o755)
            binary = root / "bin"
            binary.mkdir()
            for name, content in {
                "helm": "exit 0",
                "docker": "printf 'kindest/node:test\\n'",
                "kubectl": """case "$1" in
config) printf 'kind-fixture\\n' ;;
get) [ "$POD_CIDRS" != error ] || exit 1
     printf '{"spec":{"podCIDRs":%s}}\\n' "$POD_CIDRS" ;;
esac""",
                "kind": """printf '%s\\n' "$1" >> "$CALL_LOG"
if [ "$1" = create ]; then
  while [ "$1" != --config ]; do shift; done
  cp "$2" "$CAPTURED_CONFIG"
fi""",
            }.items():
                path = binary / name
                path.write_text("#!/bin/sh\nset -eu\n" + content + "\n")
                path.chmod(0o755)
            for family, cidrs in (("ipv4", '["10.244.0.0/24"]'),
                                  ("dual", '["10.244.0.0/24","fd00:10:244::/64"]'),
                                  ("error", "error")):
                with self.subTest(family=family):
                    calls = root / f"{family}.calls"
                    captured = root / f"{family}.yaml"
                    environment = dict(os.environ, PATH=f"{binary}:{os.environ['PATH']}",
                                       POD_CIDRS=cidrs, CALL_LOG=str(calls), CAPTURED_CONFIG=str(captured))
                    result = subprocess.run(["bash", str(root / "reset.sh")], env=environment,
                                            capture_output=True, text=True)
                    if family == "error":
                        self.assertNotEqual(result.returncode, 0)
                        self.assertFalse(calls.exists(), "cluster must survive failed discovery")
                    else:
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertEqual(calls.read_text(), "delete\ncreate\n")
                        self.assertIn(f"ipFamily: {family}\n", captured.read_text())


if __name__ == "__main__":
    unittest.main()
