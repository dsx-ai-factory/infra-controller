# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
import tempfile
import subprocess
import sys
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest import mock
import json

import lab


class LabTests(unittest.TestCase):
    def test_identity_matches_zero_dpu_bmc_and_domain(self):
        for index in (1, 2):
            with self.subTest(index=index):
                target = lab.target(index)
                xml = ET.fromstring(
                    lab.domain_xml(target, Path("/lab"), 4096, 2, "br-nico-vm")
                )
                self.assertEqual(target["bmc_mac"], f"02:00:00:{index:02x}:00:01")
                self.assertEqual(
                    xml.find("devices/interface/mac").get("address"), target["host_mac"]
                )
                self.assertEqual(
                    xml.find("sysinfo/system/entry[@name='serial']").text,
                    target["serial"],
                )
                self.assertEqual(xml.find("uuid").text, target["uuid"])
                self.assertEqual(
                    [x.get("dev") for x in xml.findall("os/boot")], ["network", "hd"]
                )
                self.assertEqual(
                    [
                        x.get("dev")
                        for x in xml.findall("devices/disk[@device='cdrom']/target")
                    ],
                    ["sdb", "sdc"],
                )

    def test_native_architecture_selects_matching_firmware_and_emulator(self):
        for arch, machine, firmware in (
            ("x86_64", "q35", "/usr/share/OVMF/OVMF_CODE_4M.fd"),
            ("aarch64", "virt", "/usr/share/AAVMF/AAVMF_CODE.fd"),
        ):
            with self.subTest(arch=arch):
                xml = ET.fromstring(
                    lab.domain_xml(
                        lab.target(1), Path("/lab"), 4096, 2, lab.BRIDGE, arch
                    )
                )
                self.assertEqual(
                    xml.find("os/type").attrib, {"arch": arch, "machine": machine}
                )
                self.assertEqual(xml.find("os/loader").text, firmware)
                self.assertEqual(
                    xml.find("devices/interface/rom").get("enabled"),
                    "yes" if arch == "x86_64" else "no",
                )
                self.assertEqual(
                    xml.find("devices/emulator").text, f"/usr/bin/qemu-system-{arch}"
                )
        with self.assertRaisesRegex(ValueError, "architecture"):
            lab.domain_xml(lab.target(1), Path("/lab"), 4096, 2, lab.BRIDGE, "s390x")

    def test_network_rules_need_only_ipv4_and_include_guest_egress(self):
        rules = list(lab.network_rules("kind0", "eth0"))
        self.assertEqual({rule[1] for rule in rules}, {"iptables"})
        self.assertIn(
            (
                "host",
                "iptables",
                "nat",
                "POSTROUTING",
                [
                    "-s",
                    lab.IPV4_SUBNET,
                    "-o",
                    "eth0",
                    "-m",
                    "comment",
                    "--comment",
                    lab.NAME,
                    "-j",
                    "MASQUERADE",
                ],
            ),
            rules,
        )

    def test_existing_inventory_cannot_be_silently_changed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = lab.configuration(2, 4096, 2, 24)
            first = lab.save_inventory(root, config)
            self.assertEqual(lab.save_inventory(root, config), first)
            with self.assertRaisesRegex(ValueError, "existing inventory"):
                lab.save_inventory(root, lab.configuration(1, 4096, 2, 24))
            self.assertEqual(lab.read_inventory(root), first)

    def test_growth_preserves_existing_target_and_rejects_shrink(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = lab.configuration(1, 3072, 3, 32)
            lab.save_inventory(root, original)
            disk = root / "guest-disk"
            disk.write_bytes(b"retained guest state")
            grown = lab.grow_inventory(root, 2)
            self.assertEqual(grown, lab.configuration(2, 3072, 3, 32))
            self.assertEqual(grown["targets"][0], original["targets"][0])
            self.assertEqual(lab.grow_inventory(root, 2), grown)
            with self.assertRaisesRegex(ValueError, "cannot remove"):
                lab.grow_inventory(root, 1)
            self.assertEqual(lab.read_inventory(root), grown)
            self.assertEqual(disk.read_bytes(), b"retained guest state")

    def test_redeployment_retains_custom_sizes_but_rejects_explicit_resize(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.assertEqual(
                lab.requested_configuration(root, 2), lab.configuration(2, 4096, 2, 24)
            )
            original = lab.configuration(2, 8192, 4, 48)
            lab.save_inventory(root, original)
            self.assertEqual(
                lab.save_inventory(root, lab.requested_configuration(root, 2)), original
            )
            with self.assertRaisesRegex(ValueError, "existing inventory"):
                lab.save_inventory(
                    root, lab.requested_configuration(root, 2, memory=4096)
                )
            self.assertEqual(lab.read_inventory(root), original)

    def test_rollout_deadline_is_not_truncated_by_process_timeout(self):
        import importlib.util

        spec = importlib.util.spec_from_file_location(
            "vm_hosts_setup", Path(lab.__file__).with_name("setup.py")
        )
        setup = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(setup)
        with mock.patch.object(lab.subprocess, "run") as run:
            setup.kubectl("rollout", "status", "deployment/nico-pxe", "--timeout=300s")
            self.assertGreater(run.call_args.kwargs["timeout"], 300)

    def test_up_resumes_only_owned_saved_guests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = lab.configuration(2, 4096, 2, 24)
            hosts = config["targets"]
            names = [host["name"] for host in hosts]
            calls = []

            def virsh(*args):
                calls.append(args)
                if args == ("list", "--all", "--name"):
                    return "\n".join(names)
                if args == ("list", "--all", "--with-managed-save", "--name"):
                    return names[0] + "\nunrelated-saved-guest"
                if args[0] == "domuuid":
                    return next(h["uuid"] for h in hosts if h["name"] == args[1])
                return ""

            with (
                mock.patch.object(lab, "preflight"),
                mock.patch.object(lab, "inspect", return_value={"owned": True}),
                mock.patch.object(lab, "owned"),
                mock.patch.object(
                    lab,
                    "container",
                    side_effect=lambda *args: calls.append(("container", args[1])),
                ),
                mock.patch.object(lab, "run"),
                mock.patch.object(lab, "virsh", side_effect=virsh),
                mock.patch("builtins.print"),
            ):
                lab.up(root, config)
            self.assertEqual(
                [call for call in calls if call[0] == "start"],
                [("start", names[0], "--paused")],
            )
            self.assertLess(
                calls.index(("start", names[0], "--paused")),
                calls.index(("container", names[0] + "-bmc")),
            )

    def test_stop_saves_guests_before_removing_network(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = lab.configuration(1, 4096, 2, 24)
            lab.save_inventory(root, config)
            name = config["targets"][0]["name"]
            calls = []

            def virsh(*args):
                calls.append(args)
                return name if args == ("list", "--name") else ""

            with (
                mock.patch.object(
                    lab, "inspect", return_value={"State": {"Running": True}}
                ),
                mock.patch.object(lab, "owned"),
                mock.patch.object(lab, "run"),
                mock.patch.object(lab, "virsh", side_effect=virsh),
                mock.patch.object(
                    lab,
                    "cleanup_network",
                    side_effect=lambda _: calls.append(("network-down",)),
                ),
            ):
                lab.stop(root)
            self.assertLess(
                calls.index(("managedsave", name)), calls.index(("network-down",))
            )

    def test_network_resume_only_unpauses_owned_matching_domains(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = lab.configuration(2, 4096, 2, 24)
            lab.save_inventory(root, config)
            host = config["targets"][0]

            def virsh(*args):
                if args[0] == "list":
                    return host["name"] + "\nunrelated-paused-guest"
                if args[0] == "domuuid":
                    return host["uuid"]
                return ""

            with mock.patch.object(lab, "virsh", side_effect=virsh) as command:
                lab.resume_paused(root)
                self.assertEqual(
                    [c.args for c in command.call_args_list if c.args[0] == "resume"],
                    [("resume", host["name"])],
                )

    def test_invalid_resources_are_rejected_before_mutation(self):
        for count, memory, cpus, disk in (
            (0, 4096, 2, 24),
            (3, 4096, 2, 24),
            (1, 0, 2, 24),
            (1, 4096, 0, 24),
            (1, 4096, 2, 0),
        ):
            with self.subTest(count=count, memory=memory, cpus=cpus, disk=disk):
                with self.assertRaises(ValueError):
                    lab.configuration(count, memory, cpus, disk)

    def test_route_overlap_rejects_ipv4_conflicts(self):
        for cidr, conflict in ((lab.IPV4_SUBNET, "10.84.0.0/16"),):
            with self.subTest(cidr=cidr):
                with self.assertRaisesRegex(ValueError, "overlaps"):
                    lab.validate_routes([{"dst": conflict}], cidr)
                lab.validate_routes([{"dst": "default"}], cidr)

    def test_network_cleanup_preserves_state_on_rule_check_failure(self):
        for returncode in (1, 4):
            with self.subTest(
                returncode=returncode
            ), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                state = root / "network-state.json"
                state.write_text(
                    json.dumps(
                        {
                            "bridge": "kind",
                            "uplink": "eth0",
                            "node": "node",
                            "routes": [],
                        }
                    )
                )
                error = subprocess.CalledProcessError(
                    returncode, ["docker", "exec"], stderr="resource problem"
                )
                with mock.patch.object(
                    lab, "inspect", return_value={"State": {"Running": True}}
                ), mock.patch.object(lab.subprocess, "run", side_effect=error):
                    with self.assertRaises(subprocess.CalledProcessError):
                        lab.cleanup_network(root)
                    self.assertTrue(state.exists())

    def test_purge_guard_rejects_retained_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lab.save_inventory(root, lab.configuration(1, 4096, 2, 24))
            result = subprocess.run(
                [
                    sys.executable,
                    str(Path(lab.__file__)),
                    "--root",
                    str(root),
                    "check-purge",
                ],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("retained disks", result.stderr)
            self.assertEqual(len(lab.read_inventory(root)["targets"]), 1)


class ProfileValuesTests(unittest.TestCase):
    def test_real_host_profile_removes_mat_auth_and_proxy(self):
        import tomllib
        import yaml
        import configure

        base = yaml.safe_load(
            (Path(__file__).parent.parent / "values.base.yaml").read_text()
        )
        result = configure.values(base, "example.test/boot:latest")
        site = tomllib.loads(result["nico-api"]["siteConfig"]["nicoApiSiteConfig"])
        self.assertFalse(site["auth"]["allow_machineatron_scout_stream"])
        self.assertFalse(site["auth"]["permissive_mode"])
        self.assertFalse(site["allow_insecure_discovery"])
        self.assertFalse(site["tpm_required"])
        self.assertNotIn("bmc_proxy", site["site_explorer"])
        proxy = tomllib.loads(
            result["nico-bmc-proxy"]["configFiles"]["nicoBmcProxyConfig"]
        )
        self.assertNotIn("bmc_proxy", proxy)
        self.assertFalse(result["nico-machine-a-tron"]["enabled"])
        self.assertFalse(result["nico-dhcp"]["dhcp"]["v6Enabled"])
        self.assertEqual(
            result["nico-pxe"]["bootArtifactContainers"][0]["imagePullPolicy"],
            "IfNotPresent",
        )

    def test_missing_boot_artifacts_fail_instead_of_a_false_ready_result(self):
        import configure

        with self.assertRaisesRegex(ValueError, "BOOT_IMAGE"):
            configure.values({}, "")


class BootArtifactTests(unittest.TestCase):
    def test_vm_profiles_keep_real_scout_and_use_matching_virtual_kernels(self):
        import build_boot_artifacts as builder

        for architecture in ("x86_64", "aarch64"):
            with self.subTest(
                architecture=architecture
            ), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                package = root / "scout.deb"
                package.write_bytes(b"test package")
                work = root / "work"
                work.mkdir()
                builder.stage_profiles(builder.REPO, work, package, architecture)
                for profile in builder.profiles(architecture):
                    config = (
                        work / "mkosi.profiles" / profile / "mkosi.conf"
                    ).read_text()
                    self.assertIn("linux-image-virtual", config)
                    self.assertIn("lsb-release", config)
                    self.assertNotIn("linux-image-6.", config)
                    self.assertNotIn("linux-modules-6.", config)
                extra = work / f"mkosi.profiles/scout-oss-{architecture}/mkosi.extra"
                self.assertFalse((extra / "etc/apt").exists())
                self.assertEqual(
                    (extra / "build-output/forge-scout.deb").read_bytes(),
                    b"test package",
                )
                self.assertEqual(
                    (extra / "etc/systemd/system/forge-scout.service").read_bytes(),
                    (
                        builder.REPO / "crates/scout/misc/forge-scout.service"
                    ).read_bytes(),
                )


if __name__ == "__main__":
    unittest.main()
