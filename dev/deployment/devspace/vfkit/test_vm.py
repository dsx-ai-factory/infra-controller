#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Host-only regression checks; real VM validation is documented separately."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import MagicMock, patch

import vm


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="nico-vfkit-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.directory = self.root / "state with spaces"
        self.directory.mkdir()
        self.machine = vm.VM(self.directory)
        self.addCleanup(shutil.rmtree, self.machine.runtime)

    def args(self, *options):
        return vm.parser().parse_args(["--vm-dir", str(self.directory), *options, "up"])

    def test_invalid_network_does_not_initialize_vm(self):
        cases = [("--network", "bridged"), ("--network", "socket"),
                 ("--interface", "en0"), ("--network-socket", "/tmp/gateway.sock"),
                 ("--network", "socket", "--network-socket", "relative.sock")]
        for options in cases:
            with self.subTest(options=options), self.assertRaises(ValueError):
                self.machine.create(self.args(*options))
            self.assertFalse(self.machine.config_path.exists())

    def test_up_prepares_native_dependencies_before_verification(self):
        machine = MagicMock()
        machine.runtime = self.machine.runtime
        with patch.object(vm, "VM", return_value=machine), \
                patch.object(vm.platform, "system", return_value="Darwin"), \
                patch.object(vm.platform, "machine", return_value="arm64"), \
                patch.object(vm.platform, "mac_ver", return_value=("26.0", (), "")):
            vm.main(["--vm-dir", str(self.directory), "up"])
        self.assertEqual([entry[0] for entry in machine.mock_calls],
                         ["create", "ensure_disk", "seed", "start", "ready", "sync",
                          "provision", "prepare_dev", "verify"])

    def test_resource_config_survives_restart_and_rejects_drift(self):
        self.machine.create(self.args("--cpus", "3", "--memory-gib", "7"))
        original = self.machine.config_path.read_bytes()
        reopened = vm.VM(self.directory)
        reopened.create(self.args())
        self.assertEqual(reopened.config["cpus"], 3)
        with self.assertRaisesRegex(ValueError, "cpus differs"):
            reopened.create(self.args("--cpus", "4"))
        self.assertEqual(self.machine.config_path.read_bytes(), original)

    def test_checksum_failure_never_publishes_root_disk(self):
        image = self.root / "image.raw"
        image.write_bytes(b"not the requested image")
        args = self.args("--image", str(image), "--image-sha256", "0" * 64)
        self.machine.create(args)
        with self.assertRaisesRegex(ValueError, "SHA256 mismatch"):
            self.machine.ensure_disk(args)
        self.assertFalse((self.directory / "root.raw").exists())
        self.assertFalse((self.directory / "image.json").exists())

    def test_missing_initialized_data_disk_is_not_recreated(self):
        data_dir = self.root / "external data"
        args = self.args("--data-dir", str(data_dir))
        self.machine.create(args)
        self.machine.config["data_initialized"] = True
        with self.assertRaisesRegex(ValueError, "refusing to replace"):
            self.machine.ensure_disk(args)
        self.assertFalse((data_dir / "data.raw").exists())
        with self.assertRaisesRegex(ValueError, "refusing to start"):
            self.machine.command()

    def test_existing_data_is_never_adopted_or_formatted(self):
        data_dir = self.root / "external data"
        data_dir.mkdir()
        disk = data_dir / "data.raw"
        disk.write_bytes(b"existing user disk")
        with self.assertRaisesRegex(ValueError, "initially be empty"):
            self.machine.create(self.args("--data-dir", str(data_dir)))
        self.assertEqual(disk.read_bytes(), b"existing user disk")
        self.assertFalse(self.machine.config_path.exists())

    def test_cloud_init_uses_standard_storage_paths(self):
        for external in (False, True):
            with self.subTest(external=external):
                files = vm.cloud_config("ssh-ed25519 fixture", "02:00:00:00:00:01", "test", external)
                user = json.loads(files["user-data"].split("\n", 1)[1])
                self.assertNotIn("/dockerroot", files["user-data"])
                paths = {item["path"]: item["content"] for item in user["write_files"]}
                self.assertNotIn("data-root", json.loads(paths["/etc/docker/daemon.json"]))
                if external:
                    self.assertEqual(user["runcmd"][0], ["/usr/local/sbin/nico-mount-data"])
                    self.assertIn("/var/lib/containerd", paths["/usr/local/sbin/nico-mount-data"])
                else:
                    self.assertNotIn("/usr/local/sbin/nico-mount-data", paths)

    def test_ssh_preserves_space_in_known_hosts_path(self):
        result = subprocess.run(self.machine.ssh_args() + ["-G", "nico@nico-vfkit"],
                                capture_output=True, text=True, check=True)
        self.assertIn(f"userknownhostsfile {self.machine.directory}/known_hosts\n", result.stdout)
        self.assertLess(len(str(self.machine.runtime / "ssh.sock")), 104)

    def test_native_build_version_comes_from_host_not_synthetic_guest_head(self):
        version = "v2.3.0-pr-146-g12345678"
        with patch.object(vm, "run", side_effect=[
                subprocess.CompletedProcess([], 0, stdout=version + "\n"),
                subprocess.CompletedProcess([], 0, stdout="12345678\n")]), \
                patch.object(self.machine, "ssh") as ssh:
            self.machine.sync_build_version()
        environment = ssh.call_args.kwargs["input"]
        result = subprocess.run(["bash", "-c", environment +
                                 'printf "%s %s" "$VERSION" "$CI_COMMIT_SHORT_SHA"'],
                                capture_output=True, text=True, check=True)
        self.assertEqual(result.stdout, version + " 12345678")

    def test_native_build_version_rejects_checkout_without_version_tag(self):
        with patch.object(vm, "run", return_value=subprocess.CompletedProcess(
                [], 0, stdout="12345678\n")), patch.object(self.machine, "ssh") as ssh:
            with self.assertRaisesRegex(ValueError, "lacks a version tag"):
                self.machine.sync_build_version()
            ssh.assert_not_called()

    def test_reused_pid_cannot_stop_another_vm(self):
        (self.machine.runtime / "vfkit.pid").write_text("123")
        with patch.object(vm, "run", return_value=subprocess.CompletedProcess(
                [], 0, stdout="vfkit --device virtio-blk,path=/another/vm/root.raw")):
            self.assertIsNone(self.machine.pid())


class NativePreparationTests(unittest.TestCase):
    def test_postgres_connection_budget_for_new_and_existing_containers(self):
        script = (vm.SCRIPT_DIR.parent / "prepare-ubuntu-host-for-dev.sh").read_text()
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
        script = (vm.SCRIPT_DIR.parent / "reset-devspace-on-host.sh").read_text()
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
            shutil.copyfile(vm.SCRIPT_DIR.parent / "reset-kind-cluster.sh", root / "reset.sh")
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
