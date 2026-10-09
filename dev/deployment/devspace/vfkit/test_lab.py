#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Managed lab lifecycle contracts without booting a VM."""

import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import lab_manager
import vm


class LabTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="nico-lab-test-")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.lab = lab_manager.Lab(self.directory / "state", 12)
        self.addCleanup(lambda: shutil.rmtree(self.lab.runtime, ignore_errors=True))

    def configure(self, uplink="nat", interface=None):
        self.lab.directory.mkdir(parents=True)
        self.lab.config_path.write_text(json.dumps({"uplink": uplink, "interface": interface,
                                                   "wan_mac": "02:00:00:00:12:01",
                                                   "lan_mac": "02:00:00:00:12:02"}))

    def test_gateway_has_private_lan_and_only_selected_wan(self):
        for uplink, interface in (("nat", None), ("bridged", "en0")):
            with self.subTest(uplink=uplink), patch.object(lab_manager, "vmnet_run", return_value="/helper"):
                self.lab.directory.mkdir(parents=True, exist_ok=True)
                self.lab.config_path.write_text(json.dumps({"uplink": uplink, "interface": interface,
                                                           "wan_mac": "02:00:00:00:12:01",
                                                           "lan_mac": "02:00:00:00:12:02"}))
                command = self.lab.command()
                devices = [command[index + 1] for index, word in enumerate(command) if word == "--device"]
                networks = [device for device in devices if device.startswith("virtio-net,")]
                self.assertEqual(len(networks), 2)
                self.assertIn(f"unixSocketPath={self.lab.socket}", networks[1])
                self.assertNotIn("nat", networks[1])
                if uplink == "bridged":
                    self.assertEqual(command[:6], ["/helper", "--operation-mode", "bridged",
                                                  "--shared-interface", "en0", "--"])
                    self.assertIn("fd=4", networks[0])
                else:
                    self.assertEqual(command[0], "vfkit")
                    self.assertIn(",nat,", networks[0])

    def test_unowned_or_symlinked_state_is_not_adopted(self):
        self.lab.root.mkdir()
        sentinel = self.lab.root / "existing-data"
        sentinel.write_text("preserve")
        with self.assertRaisesRegex(ValueError, "empty or owned"), self.lab.locked():
            self.fail("unowned root accepted")
        alias = self.directory / "alias"
        alias.symlink_to(self.lab.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "symlink"):
            lab_manager.Lab(alias, 12)
        self.assertEqual(sentinel.read_text(), "preserve")

    def test_runtime_is_short_and_independent_of_state_location(self):
        self.assertLess(len(os.fsencode(self.lab.socket)), 104)
        other = lab_manager.Lab(self.directory / "other", 12)
        self.assertNotEqual(self.lab.socket, other.socket)

    def test_saved_uplink_cannot_silently_change(self):
        self.configure("bridged", "en0")
        with self.assertRaisesRegex(ValueError, "uplink differs"):
            self.lab.ensure("nat")

    def test_stop_and_delete_refuse_clients_before_any_mutation(self):
        self.configure()
        with patch.object(self.lab, "clients", return_value=["external socket client"]), \
                patch.object(self.lab, "stop_gateway") as stop:
            for operation in (self.lab.stop, self.lab.delete):
                with self.subTest(operation=operation.__name__), self.assertRaisesRegex(ValueError, "running VM"):
                    operation()
            stop.assert_not_called()
        self.assertTrue(self.lab.config_path.exists())

    def test_client_scan_excludes_gateway_but_includes_external_socket_vms(self):
        output = (f"python3 /checkout/socket_vm.py -- vfkit --device virtio-net,unixSocketPath={self.lab.socket},mac=02:00:00:00:00:01\n"
                  f"vfkit --device virtio-net,unixSocketPath={self.lab.socket.parent}/../{self.lab.socket.parent.name}/switch.sock\n"
                  f"vfkit --device virtio-net,unixSocketPath={self.lab.socket} "
                  f"--restful-uri unix://{self.lab.runtime}/gateway.sock\n"
                  "vfkit --device virtio-net,nat\n")
        with patch.object(lab_manager, "run", return_value=subprocess.CompletedProcess([], 0, stdout=output)):
            self.assertEqual(len(self.lab.clients()), 2)
        with patch.object(lab_manager, "run", side_effect=subprocess.CalledProcessError(1, "ps")), \
                patch.object(self.lab, "stop_gateway") as stop:
            with self.assertRaises(subprocess.CalledProcessError):
                self.lab.stop()
            stop.assert_not_called()

    def test_delete_removes_only_selected_lab_and_preserves_cache_and_disks(self):
        self.configure()
        siblings = [self.lab.root / "images/cache", self.lab.root / "labs/13/data",
                    self.directory / "ubuntu/root.raw"]
        for path in siblings:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("preserve")
        with patch.object(self.lab, "clients", return_value=[]), patch.object(self.lab, "pid", return_value=None):
            self.lab.delete()
        self.assertFalse(self.lab.directory.exists())
        for path in siblings:
            self.assertEqual(path.read_text(), "preserve")

    def test_reused_pid_does_not_target_another_process(self):
        with self.lab.locked():
            (self.lab.runtime / "gateway.pid").write_text("123")
            with patch.object(lab_manager.subprocess, "run", return_value=subprocess.CompletedProcess(
                    [], 0, stdout="vfkit --restful-uri unix:///another/gateway.sock")):
                self.assertIsNone(self.lab.pid("gateway"))

    def test_switch_from_another_checkout_is_still_owned_by_this_lab(self):
        with self.lab.locked():
            (self.lab.runtime / "switch.pid").write_text("123")
            with patch.object(lab_manager.subprocess, "run", return_value=subprocess.CompletedProcess(
                    [], 0, stdout=f"python3 /another/checkout/vfkit/lab/lib/switch.py {self.lab.socket}")):
                self.assertEqual(self.lab.pid("switch"), 123)

    def test_gateway_shutdown_waits_for_guest_without_forcing_process_exit(self):
        with patch.object(self.lab, "pid", side_effect=[123, 123, None]), \
                patch.object(self.lab, "ssh") as ssh, patch.object(lab_manager.time, "sleep") as sleep:
            self.lab.stop_gateway()
        self.assertIn("exec poweroff", ssh.call_args.args[0])
        self.assertEqual(ssh.call_args.kwargs["timeout"], 10)
        sleep.assert_called_once_with(1)

    def test_delete_refuses_unexpected_files_before_stopping(self):
        self.configure()
        disk = self.lab.directory / "root.raw"
        disk.write_text("preserve")
        with patch.object(self.lab, "stop") as stop:
            with self.assertRaisesRegex(ValueError, "unexpected"):
                self.lab.delete()
            stop.assert_not_called()
        self.assertEqual(disk.read_text(), "preserve")

    def test_failed_ipv6_probe_is_not_reported_as_ready_or_fallback(self):
        with patch.object(self.lab, "ssh", side_effect=[None, subprocess.CalledProcessError(28, "curl")]) as ssh:
            with self.assertRaisesRegex(ValueError, "IPv6 HTTPS failed"):
                self.lab.verify()
        self.assertIn("curl -4", ssh.call_args_list[0].args[0])
        self.assertIn("curl -6", ssh.call_args_list[1].args[0])

    def test_configuration_reuse_does_not_rebuild_a_running_lab(self):
        self.configure()
        with patch.object(self.lab, "pid", return_value=123), patch.object(self.lab, "ready") as ready, \
                patch.object(lab_manager, "run") as run:
            self.lab.ensure()
        ready.assert_called_once()
        run.assert_not_called()

    def test_image_rebuild_refuses_assets_used_by_running_gateway(self):
        self.configure()
        spec = importlib.util.spec_from_file_location(
            "gateway_image", lab_manager.SOURCE / "image/gateway-image.py")
        builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(builder)
        assets = self.lab.root / "images" / f"gateway-{lab_manager.ALPINE_VERSION}-aarch64"
        output = subprocess.CompletedProcess([], 0, stdout=" ".join(self.lab.command()))
        with patch.object(builder, "IMAGES", assets.parent), patch.object(builder, "ASSETS", assets), \
                patch.object(builder, "run", return_value=output), patch.object(builder, "stock_iso") as iso:
            with self.assertRaisesRegex(SystemExit, "Stop diskless gateways"):
                builder.build()
            iso.assert_not_called()

    def test_standalone_environment_cannot_redirect_integrated_assets(self):
        self.configure()
        with patch.dict(os.environ, {"VXLAB_VAR": "/original/vflab", "ALPINE_VERSION": "other"}):
            environment = self.lab.environment()
        self.assertEqual(environment["VXLAB_VAR"], str(self.lab.root))
        self.assertEqual(environment["ALPINE_VERSION"], lab_manager.ALPINE_VERSION)

    def test_gateway_ip_uses_current_boot_and_expected_interface(self):
        self.lab.directory.mkdir(parents=True)
        (self.lab.directory / "serial.log").write_text(
            "OpenRC started\neth0: leased 192.0.2.1 for DHCP\nOpenRC started\n"
            "eth1: leased 10.12.0.1 for DHCP\n")
        self.assertIsNone(self.lab.gateway_ip())
        with (self.lab.directory / "serial.log").open("a") as stream:
            stream.write("eth0: leased 192.0.2.2 for DHCP\n")
        self.assertEqual(self.lab.gateway_ip(), "192.0.2.2")

    def test_delete_confirmation_is_required_before_accessing_state(self):
        with patch.object(vm.lab_manager, "Lab") as lab:
            for arguments in (["lab-delete"], ["--yes", "up"], ["--vm-dir", "/unused", "lab-stop"]):
                with self.subTest(arguments=arguments), self.assertRaises(ValueError):
                    vm.main(arguments)
            lab.assert_not_called()


if __name__ == "__main__":
    unittest.main()
