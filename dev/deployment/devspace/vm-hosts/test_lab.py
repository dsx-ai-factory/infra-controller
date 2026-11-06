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
import importlib.util


def load_setup():
    spec = importlib.util.spec_from_file_location(
        "vm_hosts_setup", Path(lab.__file__).with_name("setup.py")
    )
    setup = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(setup)
    return setup


class LabTests(unittest.TestCase):
    def test_inspect_bounds_docker_wait_and_propagates_timeout(self):
        def stalled(*args, **kwargs):
            self.assertEqual(kwargs.get("timeout"), 120)
            raise subprocess.TimeoutExpired(args[0], kwargs["timeout"])

        with mock.patch.object(lab.subprocess, "run", side_effect=stalled):
            with self.assertRaises(subprocess.TimeoutExpired):
                lab.inspect("container", "unresponsive")

    def test_network_identity_change_cleans_saved_rules_before_replacement(self):
        setup = load_setup()
        for changed in (False, True):
            with self.subTest(
                changed=changed
            ), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path = root / "network-state.json"
                saved = dict(
                    node="node",
                    bridge="old-bridge",
                    uplink="old-uplink",
                    routes=[["4", "192.0.2.1/32", "192.0.2.2"]],
                )
                path.write_text(json.dumps(saved))
                bridge = "new-bridge" if changed else saved["bridge"]
                uplink = "new-uplink" if changed else saved["uplink"]
                calls = []

                def command(*args):
                    calls.append(args)
                    if "-json" in args:
                        return json.dumps([{"gateway": "192.0.2.2"}])
                    return "present"

                with mock.patch.object(
                    lab, "inspect", return_value={"State": {"Running": True}}
                ), mock.patch.object(lab, "run", side_effect=command):
                    result = setup.network_state(root, "node", bridge, uplink)
                self.assertEqual(json.loads(path.read_text()), result)
                if changed:
                    self.assertEqual(
                        result,
                        dict(node="node", bridge=bridge, uplink=uplink, routes=[]),
                    )
                    deletions = [args for args in calls if "-D" in args]
                    self.assertEqual(
                        len(deletions),
                        len(list(lab.network_rules(saved["bridge"], saved["uplink"]))),
                    )
                    self.assertTrue(any("old-uplink" in args for args in deletions))
                    self.assertFalse(any("new-uplink" in args for args in deletions))
                    self.assertIn(
                        (
                            "docker",
                            "exec",
                            lab.NAME,
                            "ip",
                            "-4",
                            "route",
                            "del",
                            "192.0.2.1/32",
                            "via",
                            "192.0.2.2",
                        ),
                        calls,
                    )
                else:
                    self.assertEqual(result, saved)
                    self.assertEqual(calls, [])

    def test_network_identity_cleanup_failure_retains_saved_state(self):
        setup = load_setup()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "network-state.json"
            original = json.dumps(
                dict(node="old", bridge="old", uplink="old", routes=[])
            )
            path.write_text(original)
            with mock.patch.object(
                lab, "cleanup_network", side_effect=ValueError("cleanup failed")
            ):
                with self.assertRaisesRegex(ValueError, "cleanup failed"):
                    setup.network_state(root, "new", "new", "new")
            self.assertEqual(path.read_text(), original)

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
                # bmc-mock creates and owns its virtual-media devices at startup.
                self.assertEqual(
                    [disk.get("device") for disk in xml.findall("devices/disk")],
                    ["disk"],
                )
        with self.assertRaisesRegex(ValueError, "architecture"):
            lab.domain_xml(lab.target(1), Path("/lab"), 4096, 2, lab.BRIDGE, "s390x")

    def test_network_rules_forward_both_families_and_nat_only_ipv4(self):
        rules = list(lab.network_rules("kind0", "eth0"))
        self.assertEqual({rule[1] for rule in rules}, {"iptables", "ip6tables"})
        self.assertFalse(
            any(
                rule[1] == "ip6tables" and rule[4][-1] in ("MASQUERADE", "SNAT")
                for rule in rules
            )
        )
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
                mock.patch.object(
                    lab, "run", side_effect=lambda *args: calls.append(args)
                ),
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
            for suffix in ("dhcp4", "dhcp6", "ra"):
                relay = f"{lab.NAME}-{suffix}"
                self.assertLess(
                    calls.index(("managedsave", name)),
                    calls.index(("docker", "stop", relay)),
                )
                self.assertIn(("docker", "rm", relay), calls)

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

    def test_route_overlap_rejects_both_address_families(self):
        for cidr, conflict in (
            (lab.IPV4_SUBNET, "10.84.0.0/16"),
            (lab.IPV6_SUBNET, "fd42:84:30::10/128"),
        ):
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
        self.assertTrue(result["nico-dhcp"]["dhcp"]["v6Enabled"])
        self.assertEqual(
            result["nico-pxe"]["bootArtifactContainers"][0]["imagePullPolicy"],
            "IfNotPresent",
        )

    def test_missing_boot_artifacts_fail_instead_of_a_false_ready_result(self):
        import configure

        with self.assertRaisesRegex(ValueError, "BOOT_IMAGE"):
            configure.values({}, "")


class BootArtifactTests(unittest.TestCase):
    def test_scout_dependency_check_rejects_ldd_failure_and_missing_libraries(self):
        script = Path(lab.__file__).with_name("scout-vm-postinst.sh").read_text()
        # Run the shipped dependency-check block without provisioning a root image.
        check = (
            script[script.index("if ldd ") :]
            if "if ldd " in script
            else script[script.index("scout_deps=") :]
        )
        for output, code, passed in (
            ("libc => /lib/libc.so", 0, True),
            ("libbad => not found", 0, False),
            ("ldd failed", 1, False),
        ):
            with self.subTest(output=output):
                command = (
                    "set -euo pipefail\nldd() { printf '%s\\n' "
                    + repr(output)
                    + "; return "
                    + str(code)
                    + "; }\n"
                    + check
                )
                result = subprocess.run(
                    ["bash", "-c", command], capture_output=True, text=True
                )
                self.assertEqual(result.returncode == 0, passed, result.stderr)

    def test_output_context_excludes_retained_build_workspace(self):
        import build_boot_artifacts as builder

        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            (repo / "pxe/mkosi/bin").mkdir(parents=True)
            (repo / "pxe/mkosi/bin/mkosi").touch()
            package = repo / "scout.deb"
            package.touch()
            boot = repo / "boot"
            boot.mkdir()
            for name in ("ipxe.efi", "qcow-imager.efi"):
                (boot / name).write_bytes(b"payload")
            output = repo / "artifacts"

            def build(*args, **kwargs):
                built = output / "build/output"
                built.mkdir(exist_ok=True)
                for name in ("loader.efi", "scout.squashfs"):
                    (built / name).write_bytes(b"built payload")

            with mock.patch.object(builder, "REPO", repo), mock.patch.object(
                builder.lab, "native_architecture", return_value="x86_64"
            ), mock.patch.object(
                builder.platform, "system", return_value="Linux"
            ), mock.patch.object(
                builder.os, "geteuid", return_value=0
            ), mock.patch.object(
                builder, "stage_profiles"
            ), mock.patch.object(
                builder.subprocess,
                "check_output",
                side_effect=["amd64", "1.0", "revision"],
            ), mock.patch.object(
                builder.subprocess, "run", side_effect=build
            ), mock.patch.object(
                sys,
                "argv",
                [
                    "builder",
                    "--scout-package",
                    str(package),
                    "--boot-dir",
                    str(boot),
                    "--output",
                    str(output),
                ],
            ), mock.patch(
                "builtins.print"
            ):
                builder.main()
            self.assertEqual((output / ".dockerignore").read_text(), "build\n")
            self.assertTrue((output / "build").is_dir())
            for name in ("ipxe.efi", "qcow-imager.efi", "scout.efi", "scout.squashfs"):
                self.assertTrue((output / "x86_64" / name).stat().st_size)
            self.assertEqual(
                json.loads((output / "build-info.json").read_text())["source_revision"],
                "revision",
            )

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
