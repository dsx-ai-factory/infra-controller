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

    def test_ubuntu_manifest_must_be_signed_and_match_the_pin_before_image_download(self):
        self.machine.create(self.args())

        def download(args, **kwargs):
            self.assertEqual(args[0], "curl")
            self.assertIn(args[-1].rsplit("/", 1)[1], ("SHA256SUMS", "SHA256SUMS.gpg"))
            Path(args[args.index("--output") + 1]).write_text(
                "0" * 64 + " *" + vm.IMAGE_URL.rsplit("/", 1)[1] + "\n")

        cases = [(ValueError("invalid signature"), "invalid signature"),
                 (None, "pinned image SHA256")]
        for failure, message in cases:
            with self.subTest(message=message), patch.object(vm.shutil, "which", return_value="/tool"), \
                    patch.object(vm, "run", side_effect=download), \
                    patch.object(vm, "verify_signature", side_effect=failure) as verify, \
                    self.assertRaisesRegex(ValueError, message):
                self.machine.ensure_disk(self.args())
            verify.assert_called_once()
            self.assertEqual(verify.call_args.args[2], vm.IMAGE_SIGNING_KEY)
            self.assertFalse((self.directory / "root.raw").exists())
            self.assertFalse((self.directory / "image.json").exists())

    def test_custom_image_is_retained_across_interrupted_creation(self):
        source = self.root / 'custom.raw'
        source.write_bytes(b'custom image')
        checksum = vm.digest(source)
        self.machine.create(self.args('--image', str(source), '--image-sha256', checksum.upper()))
        reopened = vm.VM(self.directory)
        reopened.create(self.args())
        reopened.ensure_disk(self.args())
        with (self.directory / 'root.raw').open('rb') as disk:
            self.assertEqual(disk.read(12), b'custom image')
        for options in (('--image', str(self.root / 'other.raw'), '--image-sha256', checksum),
                        ('--image', str(source), '--image-sha256', '0' * 64)):
            with self.subTest(options=options), self.assertRaisesRegex(ValueError, 'differs'):
                reopened.create(self.args(*options))

    def test_socket_requires_control_companion_before_launch(self):
        self.machine.create(self.args('--network', 'socket', '--network-socket', '/tmp/fixture.sock'))
        (self.directory / 'root.raw').touch()
        with patch.object(vm.Path, 'is_socket', side_effect=[True, False]), self.assertRaisesRegex(ValueError, 'data/control'):
            self.machine.command()

    def test_socket_modes_launch_supervisor_but_native_networks_do_not(self):
        (self.directory / "root.raw").touch()
        for network in ("lab", "socket", "nat", "bridged"):
            with self.subTest(network=network):
                self.machine.config = {"network": network}
                with patch.object(self.machine, "pid", side_effect=[None, 123]), \
                        patch.object(self.machine, "command", return_value=["vfkit", "test-device"]), \
                        patch.object(vm.subprocess, "Popen") as launch:
                    self.machine.start_process()
                command = launch.call_args.args[0]
                if network in ("lab", "socket"):
                    self.assertTrue(command[1].endswith("/lib/socket_vm.py"))
                    self.assertEqual(command[2:], ["--", "vfkit", "test-device"])
                else:
                    self.assertEqual(command, ["vfkit", "test-device"])

    def test_invalid_network_does_not_initialize_vm(self):
        cases = [("--network", "bridged"), ("--network", "socket"),
                 ("--interface", "en0"), ("--network-socket", "/tmp/gateway.sock"),
                 ("--network", "socket", "--network-socket", "relative.sock")]
        for options in cases:
            with self.subTest(options=options), self.assertRaises(ValueError):
                self.machine.create(self.args(*options))
            self.assertFalse(self.machine.config_path.exists())

    def test_up_boots_and_syncs_without_deploying_a_stack(self):
        machine = MagicMock()
        machine.runtime = self.machine.runtime
        with patch.object(vm, "VM", return_value=machine), \
                patch.object(vm, "check_vfkit_version") as check_version, \
                patch.object(vm.platform, "system", return_value="Darwin"), \
                patch.object(vm.platform, "machine", return_value="arm64"), \
                patch.object(vm.platform, "mac_ver", return_value=("26.0", (), "")):
            vm.main(["--vm-dir", str(self.directory), "up"])
        check_version.assert_called_once_with()
        self.assertEqual([entry[0] for entry in machine.mock_calls],
                         ["create", "ensure_disk", "seed", "start", "ready", "sync",
                          "verify"])

    def test_resource_config_survives_restart_and_rejects_drift(self):
        self.machine.create(self.args("--cpus", "3", "--memory-gib", "7"))
        original = self.machine.config_path.read_bytes()
        reopened = vm.VM(self.directory)
        reopened.create(self.args())
        self.assertEqual(reopened.config["cpus"], 3)
        with self.assertRaisesRegex(ValueError, "cpus differs"):
            reopened.create(self.args("--cpus", "4"))
        self.assertEqual(self.machine.config_path.read_bytes(), original)

    def test_default_resources_use_internal_16_gib_memory_200_gib_disk(self):
        self.machine.create(self.args())
        self.assertEqual(self.machine.config["memory_gib"], 16)
        self.assertEqual(self.machine.config["disk_gib"], 200)
        self.assertIsNone(self.machine.config["data_dir"])

    def test_vm_command_enables_nested_virtualization(self):
        self.machine.create(self.args())
        for network in ("nat", "bridged"):
            with self.subTest(network=network), \
                    patch.object(vm.lab_manager, "vmnet_run", return_value="/helper"):
                self.machine.config.update(network=network, interface="en0")
                command = self.machine.command()
                self.assertEqual(command.count("--nested"), 1)
                self.assertGreater(command.index("--nested"), command.index("vfkit"))

    def test_lab_configuration_survives_restart_and_rejects_drift(self):
        self.machine.create(self.args("--network", "lab", "--lab-id", "12",
                                     "--lab-uplink", "bridged", "--lab-interface", "en0",
                                     "--lab-root", str(self.root / "labs")))
        config = self.machine.config.copy()
        reopened = vm.VM(self.directory)
        reopened.create(self.args())
        self.assertEqual(reopened.config, config)
        self.assertEqual(config["lab_id"], 12)
        self.assertEqual(config["lab_uplink"], "bridged")
        with self.assertRaisesRegex(ValueError, "lab_id differs"):
            reopened.create(self.args("--lab-id", "13"))

    def test_lab_options_reject_ambiguous_attachment(self):
        cases = [("--lab-id", "1"), ("--network", "lab", "--interface", "en0"),
                 ("--network", "lab", "--network-socket", "/tmp/switch.sock"),
                 ("--network", "lab", "--lab-uplink", "bridged"),
                 ("--network", "lab", "--lab-interface", "en0")]
        for options in cases:
            with self.subTest(options=options), self.assertRaises(ValueError):
                self.machine.create(self.args(*options))
            self.assertFalse(self.machine.config_path.exists())

    def test_lab_storage_must_be_separate_from_vm_disks(self):
        for disk in ("root", "data"):
            with self.subTest(disk=disk):
                lab_root = self.directory if disk == "root" else self.root / "lab-state"
                options = ["--network", "lab", "--lab-root", str(lab_root)]
                if disk == "data":
                    options += ["--data-dir", str(lab_root / "labs/1/data")]
                with self.assertRaisesRegex(ValueError, "separate from managed lab"):
                    self.machine.create(self.args(*options))
                self.assertFalse(self.machine.config_path.exists())

    def test_readiness_probes_preserve_exec_streams(self):
        with patch.object(self.machine, "pid", return_value=123), \
                patch.object(self.machine, "ssh") as ssh:
            self.machine.ready()
        self.assertEqual(len(ssh.call_args_list), 2)
        for call in ssh.call_args_list:
            self.assertEqual(call.kwargs["stdin"], subprocess.DEVNULL)
        self.assertIs(ssh.call_args_list[-1].kwargs["stdout"], vm.sys.stderr)

    def test_sync_preserves_caller_input_but_streams_the_archive(self):
        archive = MagicMock()
        archive.wait.return_value = 0
        launch = MagicMock()
        launch.__enter__.return_value = archive
        link = b"160000 " + b"1" * 40 + b" 0\tpxe/tool\0"
        with patch.object(self.machine, "ssh", return_value=subprocess.CompletedProcess([], 0, stdout=b"")) as ssh, \
                patch.object(self.machine, "sync_build_version"), \
                patch.object(vm, "host_git", side_effect=[subprocess.CompletedProcess([], 0, stdout=link),
                                                         subprocess.CompletedProcess([], 0, stdout=b"")]), \
                patch.object(vm.subprocess, "Popen", return_value=launch):
            self.machine.sync()
        self.assertEqual(len(ssh.call_args_list), 5)
        self.assertEqual(ssh.call_args_list[3].kwargs["stdin"], subprocess.DEVNULL)
        self.assertEqual(ssh.call_args_list[4].kwargs["input"], link)
        self.assertEqual(ssh.call_args_list[0].kwargs["stdin"], subprocess.DEVNULL)
        self.assertIs(ssh.call_args_list[1].kwargs["stdin"], archive.stdout)
        self.assertEqual(ssh.call_args_list[2].kwargs["stdin"], subprocess.DEVNULL)
        archive.stdout.close.assert_called_once()

    def test_conflicted_submodule_blocks_sync_before_guest_mutation(self):
        conflict = b"160000 " + b"1" * 40 + b" 2\tpxe/tool\0"
        with patch.object(vm, 'host_git', return_value=subprocess.CompletedProcess([], 0, stdout=conflict)), \
                patch.object(self.machine, 'ssh') as ssh, patch.object(vm.subprocess, 'Popen') as tar, \
                self.assertRaisesRegex(ValueError, 'index conflicts'):
            self.machine.sync()
        ssh.assert_not_called()
        tar.assert_not_called()

    def test_synced_gitlink_can_initialize_pinned_submodule_without_host_git_metadata(self):
        source, dependency, guest = (self.root / name for name in ('source', 'dependency', 'guest'))
        environment = {name: value for name, value in os.environ.items() if not name.startswith('GIT_')}
        def git(directory, *args):
            return subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', '-C', str(directory), *args], check=True,
                                  env=environment, capture_output=True, text=True)
        for directory in (source, dependency, guest):
            directory.mkdir()
            git(directory, 'init', '-q')
            git(directory, '-c', 'commit.gpgsign=false', '-c', 'user.name=Fixture',
                '-c', 'user.email=fixture@example.invalid', 'commit', '-q', '--allow-empty', '-m', 'fixture')
        (dependency / 'payload').write_text('pinned payload')
        git(dependency, 'add', 'payload')
        git(dependency, '-c', 'commit.gpgsign=false', '-c', 'user.name=Fixture',
            '-c', 'user.email=fixture@example.invalid', 'commit', '-q', '-m', 'payload')
        git(source, '-c', 'protocol.file.allow=always', 'submodule', 'add', str(dependency), 'pxe/tool')
        shutil.copyfile(source / '.gitmodules', guest / '.gitmodules')
        def guest_command(*args, **kwargs):
            kwargs.setdefault('capture_output', True)
            return subprocess.run([args[0], '-c', 'core.hooksPath=/dev/null', *args[1:]],
                                  check=True, env=environment, **kwargs)
        with patch.object(vm, 'REPO', source), patch.object(vm, 'GUEST_REPO', str(guest)), \
                patch.object(self.machine, 'ssh', side_effect=guest_command):
            self.machine.sync_submodules(vm.source_gitlinks())
        git(guest, '-c', 'protocol.file.allow=always', 'submodule', 'update', '--init', 'pxe/tool')
        self.assertEqual((guest / 'pxe/tool/payload').read_text(), 'pinned payload')
        self.assertEqual(git(guest / 'pxe/tool', 'rev-parse', 'HEAD').stdout,
                         git(dependency, 'rev-parse', 'HEAD').stdout)
        git(source, 'update-index', '--force-remove', '--', 'pxe/tool')
        (guest / '.gitmodules').write_text('')
        with patch.object(vm, 'REPO', source), patch.object(vm, 'GUEST_REPO', str(guest)), \
                patch.object(self.machine, 'ssh', side_effect=guest_command):
            self.machine.sync_submodules(vm.source_gitlinks())
        self.assertEqual(git(guest, 'ls-files', '--stage').stdout, '')
        git(guest, '-c', 'protocol.file.allow=always', 'submodule', 'update', '--init')
        self.assertTrue((guest / 'pxe/tool/payload').exists())


    def test_guest_shell_loads_source_version_and_allows_recovery_before_sync(self):
        version = self.root / '.config/nico/vfkit-source-version.sh'
        version.parent.mkdir(parents=True)
        version.write_text('export VERSION=v1.2.3-4-gabcdef00\nexport CI_COMMIT_SHORT_SHA=abcdef00\n')
        checkout = self.root / 'checkout'
        for initialized in (False, True):
            if initialized: checkout.mkdir()
            with self.subTest(initialized=initialized), patch.object(vm, 'GUEST_REPO', str(checkout)):
                result = subprocess.run(['bash', '-c', vm.guest_shell_setup() +
                                         'printf "%s %s %s" "$PWD" "$VERSION" "$CI_COMMIT_SHORT_SHA"'],
                                        env=dict(os.environ, HOME=str(self.root)), check=True, capture_output=True, text=True)
            expected = checkout if initialized else self.root
            self.assertEqual(result.stdout, f'{expected} v1.2.3-4-gabcdef00 abcdef00')
            self.assertEqual('checkout unavailable' in result.stderr, not initialized)

    def test_vm_verification_preserves_caller_input(self):
        self.machine.config = {"network": "lab"}
        with patch.object(self.machine, "ssh") as ssh:
            self.machine.verify()
        self.assertEqual(len(ssh.call_args_list), 3)
        for call in ssh.call_args_list[1:]:
            self.assertEqual(call.kwargs["stdin"], subprocess.DEVNULL)

    def test_failed_cloud_init_blocks_setup_but_not_recovery_access(self):
        for required in (True, False):
            with self.subTest(require_cloud_init=required), patch.object(self.machine, "pid", return_value=123), \
                    patch.object(self.machine, "ssh", side_effect=[None, subprocess.CalledProcessError(2, "cloud-init")]) as ssh:
                if required:
                    with self.assertRaises(subprocess.CalledProcessError):
                        self.machine.ready()
                else:
                    self.machine.ready(require_cloud_init=False)
                    ssh.assert_called_once()
                    self.assertEqual(ssh.call_args.args, ("true",))

    def test_public_recovery_commands_do_not_wait_for_cloud_init(self):
        for action in ("ssh", "exec", "verify"):
            machine = MagicMock()
            machine.runtime = self.machine.runtime
            machine.ssh_args.return_value = ['ssh']
            args = ["--vm-dir", str(self.directory), action]
            if action == "exec": args += ["--", "true"]
            with self.subTest(action=action), patch.object(vm, "VM", return_value=machine), \
                    patch.object(vm, "check_vfkit_version"), patch.object(vm, "run") as command_run, \
                    patch.object(vm.platform, "system", return_value="Darwin"), \
                    patch.object(vm.platform, "machine", return_value="arm64"), \
                    patch.object(vm.platform, "mac_ver", return_value=("26.0", (), "")):
                vm.main(args)
            machine.ready.assert_called_once_with(require_cloud_init=action == "verify")
            if action == 'ssh':
                self.assertEqual(command_run.call_args.args[0][-1], vm.guest_shell_setup() + 'exec bash -l')
            elif action == 'exec':
                self.assertEqual(machine.ssh.call_args.args, ('bash', '-lc', vm.guest_shell_setup() + 'exec true'))

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
                files = vm.cloud_config(["ssh-ed25519 fixture"], "02:00:00:00:00:01", "test", external)
                user = json.loads(files["user-data"].split("\n", 1)[1])
                self.assertNotIn("/dockerroot", files["user-data"])
                paths = {item["path"]: item["content"] for item in user["write_files"]}
                self.assertNotIn("data-root", json.loads(paths["/etc/docker/daemon.json"]))
                if external:
                    self.assertEqual(user["runcmd"][0], ["/usr/local/sbin/nico-mount-data"])
                    self.assertIn("/var/lib/containerd", paths["/usr/local/sbin/nico-mount-data"])
                else:
                    self.assertNotIn("/usr/local/sbin/nico-mount-data", paths)

    def test_ssh_preserves_unicode_and_space_in_known_hosts_path(self):
        self.machine.directory = self.root / "état with spaces"
        result = subprocess.run(self.machine.ssh_args() + ["-G", "nico@nico-vfkit"],
                                capture_output=True, text=True, check=True)
        self.assertIn(f"userknownhostsfile {self.machine.directory}/known_hosts\n", result.stdout)
        self.assertLess(len(str(self.machine.runtime / "ssh.sock")), 104)

    def test_seed_authorizes_local_public_keys_without_copying_private_keys(self):
        self.machine.create(self.args())
        generated = self.directory / "id_ed25519"
        generated.write_text("PRIVATE VM KEY")
        generated.with_suffix(".pub").write_text("ssh-ed25519 generated\n")
        home = self.root / "home"
        ssh = home / ".ssh"
        with patch.object(vm.Path, "home", return_value=home):
            self.machine.seed()
            user = json.loads((self.directory / "user-data").read_text().split("\n", 1)[1])
            self.assertEqual(user["users"][0]["ssh_authorized_keys"], ["ssh-ed25519 generated"])

            ssh.mkdir(parents=True)
            (ssh / "id_ed25519").write_text("PRIVATE USER KEY")
            (ssh / "misnamed.pub").write_text("-----BEGIN OPENSSH PRIVATE KEY-----\nPRIVATE\n")
            (ssh / "id_ed25519.pub").write_text("\n# comment\nssh-ed25519 local user\n")
            (ssh / "duplicate.pub").write_text("ssh-ed25519 local user\n")
            (ssh / "hardware.pub").write_text("sk-ssh-ed25519@openssh.com hardware user\n")
            self.machine.seed()
        user = json.loads((self.directory / "user-data").read_text().split("\n", 1)[1])
        self.assertEqual(user["users"][0]["ssh_authorized_keys"],
                         ["ssh-ed25519 generated", "ssh-ed25519 local user",
                          "sk-ssh-ed25519@openssh.com hardware user"])
        self.assertNotIn("PRIVATE", (self.directory / "user-data").read_text())

    def test_recovery_socket_is_enabled_before_package_installation(self):
        user = json.loads(vm.cloud_config(["ssh-ed25519 fixture"], "02:00:00:00:00:01", "test")["user-data"].split("\n", 1)[1])
        self.assertTrue(user["package_update"])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "bin"
            binary.mkdir()
            systemctl = binary / "systemctl"
            systemctl.write_text('#!/bin/sh\ntest -f "$UNIT_ROOT/nico-ssh.socket" && test -f "$UNIT_ROOT/nico-ssh@.service" || exit 41\nprintf "%s\\n" "$*" >> "$UNIT_ROOT/events"\n')
            systemctl.chmod(0o755)
            for command in user["bootcmd"]:
                isolated = [arg.replace('/etc/systemd/system', str(root / 'units')) for arg in command]
                subprocess.run(isolated, check=True, env=dict(os.environ, PATH=str(binary) + os.pathsep + os.environ['PATH'], UNIT_ROOT=str(root / 'units')))
            self.assertEqual((root / 'units/events').read_text().splitlines(),
                             ['daemon-reload', 'enable --now --no-block nico-ssh.socket'])
            self.assertIn('ListenStream=vsock::22', (root / 'units/nico-ssh.socket').read_text())
            self.assertIn('ExecStart=/usr/sbin/sshd -i', (root / 'units/nico-ssh@.service').read_text())
        self.assertFalse(any(entry['path'].endswith(('nico-ssh.socket', 'nico-ssh@.service')) for entry in user['write_files']))

    def test_cloud_init_configures_ten_ssh_authentication_attempts(self):
        files = vm.cloud_config(["ssh-ed25519 fixture"], "02:00:00:00:00:01", "test")
        user = json.loads(files["user-data"].split("\n", 1)[1])
        paths = {item["path"]: item["content"] for item in user["write_files"]}
        self.assertEqual(paths["/etc/ssh/sshd_config.d/00-nico-dev.conf"], "MaxAuthTries 10\n")
        self.assertIn(["systemctl", "try-reload-or-restart", "ssh.service"], user["runcmd"])
        self.assertFalse(user["ssh_pwauth"])

    def test_nat_mtu_survives_dhcp_and_router_advertisements(self):
        for mode in ("nat", "bridged", "socket"):
            with self.subTest(mode=mode):
                files = vm.cloud_config(["ssh-ed25519 fixture"], "02:00:00:00:00:01",
                                        "test", network_mode=mode)
                network = json.loads(files["network-config"])["ethernets"]["eth0"]
                user = json.loads(files["user-data"].split("\n", 1)[1])
                dropins = [entry for entry in user["write_files"]
                           if entry["path"].endswith("10-nico-mtu.conf")]
                if mode == "nat":
                    self.assertEqual(network["mtu"], 1280)
                    for family in ("dhcp4", "dhcp6"):
                        self.assertFalse(network[f"{family}-overrides"]["use-mtu"])
                    self.assertEqual(len(dropins), 1)
                    self.assertEqual(dropins[0]["content"], "[IPv6AcceptRA]\nUseMTU=no\n")
                    self.assertEqual(user["runcmd"][-2:],
                                     [["networkctl", "reload"],
                                      ["networkctl", "reconfigure", "eth0"]])
                else:
                    self.assertNotIn("mtu", network)
                    self.assertFalse(dropins)

    def test_build_versions_come_from_host_not_synthetic_guest_head(self):
        version = "v2.3.0-pr-146-g12345678"
        with patch.object(vm, "host_git", side_effect=[
                subprocess.CompletedProcess([], 0, stdout=version + "\n"),
                subprocess.CompletedProcess([], 0, stdout="12345678\n")]), \
                patch.object(self.machine, "ssh") as ssh:
            self.machine.sync_build_version()
        environment = ssh.call_args.kwargs["input"]
        result = subprocess.run(["bash", "-c", environment +
                                 'printf "%s\\n" "$VERSION" "$CI_COMMIT_SHORT_SHA"'],
                                capture_output=True, text=True, check=True)
        self.assertEqual(result.stdout.splitlines(), [version, "12345678"])

    def test_native_build_version_rejects_checkout_without_version_tag(self):
        with patch.object(vm, "host_git", return_value=subprocess.CompletedProcess(
                [], 0, stdout="12345678\n")), patch.object(self.machine, "ssh") as ssh:
            with self.assertRaisesRegex(ValueError, "lacks a version tag"):
                self.machine.sync_build_version()
            ssh.assert_not_called()

    def test_missing_pidfile_finds_live_disk_owner_and_blocks_duplicate_start(self):
        command = f"321 /opt/homebrew/bin/vfkit --device virtio-blk,path={self.machine.directory}/root.raw\n"
        with patch.object(vm, 'run', return_value=subprocess.CompletedProcess([], 0, stdout=command)), patch.object(vm.subprocess, 'Popen') as start:
            self.assertEqual(self.machine.pid(), 321)
            self.machine.start_process()
            start.assert_not_called()
        with patch.object(vm, 'run', return_value=subprocess.CompletedProcess([], 0, stdout=command + command.replace('321 ', '322 '))):
            with self.assertRaisesRegex(ValueError, 'multiple processes'):
                self.machine.pid()

    def test_process_identity_does_not_match_a_disk_path_suffix(self):
        command = f"321 /opt/homebrew/bin/vfkit --device virtio-blk,path=/Volumes/external{self.machine.directory}/root.raw\n"
        with patch.object(vm, 'run', return_value=subprocess.CompletedProcess([], 0, stdout=command)):
            self.assertIsNone(self.machine.pid())

    def test_reused_pid_cannot_stop_another_vm(self):
        (self.machine.runtime / "vfkit.pid").write_text("123")
        with patch.object(vm, "run", return_value=subprocess.CompletedProcess(
                [], 0, stdout="vfkit --device virtio-blk,path=/another/vm/root.raw")):
            self.assertIsNone(self.machine.pid())


class HostPreflightTests(unittest.TestCase):
    def test_supported_vfkit_versions(self):
        for version in ("v0.6.2", "v0.10.0"):
            with self.subTest(version=version), patch.object(vm, "run", return_value=
                    subprocess.CompletedProcess([], 0, stdout=f"vfkit version: {version}\n")) as run:
                vm.check_vfkit_version()
                run.assert_called_once_with(["vfkit", "--version"], capture_output=True, text=True)

    def test_unsupported_vfkit_rejected_before_accessing_vm_state(self):
        for action, version in (("up", "v0.6.1"), ("start", "development")):
            with self.subTest(action=action), tempfile.TemporaryDirectory() as temporary, \
                    patch.object(vm.platform, "system", return_value="Darwin"), \
                    patch.object(vm.platform, "machine", return_value="arm64"), \
                    patch.object(vm.platform, "mac_ver", return_value=("26.0", (), "")), \
                    patch.object(vm, "VM") as machine, \
                    patch.object(vm, "run", return_value=subprocess.CompletedProcess(
                        [], 0, stdout=f"vfkit version: {version}\n")):
                directory = Path(temporary) / "state"
                with self.assertRaisesRegex(ValueError, "vfkit 0.6.2 or newer is required"):
                    vm.main(["--vm-dir", str(directory), action])
                machine.assert_not_called()
                self.assertFalse(directory.exists())

    def test_host_git_ignores_foreign_repository_environment(self):
        with tempfile.TemporaryDirectory(prefix="nico-git-env-test-") as temporary:
            root = Path(temporary)
            # Keep fixture creation independent of the invoking shell and user hooks.
            environment = {name: value for name, value in os.environ.items()
                           if not name.startswith("GIT_")}

            def git(repo, *args):
                return subprocess.run(["git", "-C", str(repo), *args], check=True,
                                      env=environment, capture_output=True, text=True).stdout

            checkout, foreign = root / "checkout", root / "foreign"
            for repo, tag in ((checkout, "v1.0.0"), (foreign, "v9.0.0")):
                repo.mkdir()
                git(repo, "init", "-q")
                (repo / repo.name).write_text(tag)
                git(repo, "add", repo.name)
                git(repo, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                    "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgSign=false",
                    "commit", "-qm", "Fixture")
                git(repo, "-c", "tag.gpgSign=false", "tag", tag)
            (checkout / "untracked").write_text("local edits")
            commands = [("ls-files", "-z", "--cached", "--others", "--exclude-standard"),
                        ("describe", "--tags", "--first-parent", "--always", "--long"),
                        ("rev-parse", "--short=8", "HEAD")]
            expected = [git(checkout, *command) for command in commands]
            overrides = {"GIT_DIR": str(foreign / ".git"), "GIT_WORK_TREE": str(foreign),
                         "GIT_COMMON_DIR": str(foreign / ".git"),
                         "GIT_INDEX_FILE": str(foreign / ".git/index"),
                         "GIT_OBJECT_DIRECTORY": str(foreign / ".git/objects"),
                         "GIT_ALTERNATE_OBJECT_DIRECTORIES": str(foreign / ".git/objects")}
            with patch.dict(os.environ, dict(environment, **overrides), clear=True), \
                    patch.object(vm, "REPO", checkout):
                actual = [vm.host_git(*command, capture_output=True, text=True).stdout
                          for command in commands]
                self.assertEqual(actual, expected)
                self.assertEqual(os.environ["GIT_DIR"], overrides["GIT_DIR"])


if __name__ == "__main__":
    unittest.main()
