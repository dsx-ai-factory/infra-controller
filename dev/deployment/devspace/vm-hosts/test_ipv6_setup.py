# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
import copy
import json
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import configure_ipv6 as ipv6
import validate_ipv6_only as validator


class IPv6SetupTests(unittest.TestCase):
    def setUp(self):
        self.addresses = {
            "nico-api": "fd00::10",
            "nico-pxe": "fd00::20",
            "kube-dns": "fd00::53",
        }

    def test_companion_preserves_selectors_and_boot_ports_without_node_ports(self):
        service = {
            "metadata": {"name": "nico-pxe", "namespace": "nico-system"},
            "spec": {
                "selector": {"app": "pxe"},
                "ports": [
                    {
                        "name": "http",
                        "port": 8080,
                        "targetPort": 8080,
                        "nodePort": 30080,
                    },
                    {"name": "http-boot", "port": 80, "targetPort": 8080},
                ],
                "clusterIP": "10.0.0.1",
            },
        }
        original = copy.deepcopy(service)
        result = ipv6.companion(service)
        self.assertEqual(
            result["spec"],
            {
                "ipFamilies": ["IPv6"],
                "ipFamilyPolicy": "SingleStack",
                "selector": {"app": "pxe"},
                "ports": [
                    {"name": "http", "port": 8080, "targetPort": 8080},
                    {"name": "http-boot", "port": 80, "targetPort": 8080},
                ],
            },
        )
        self.assertEqual(service, original)

    def test_dns_preserves_ipv4_and_unrelated_blocks_and_replaces_old_ipv6(self):
        outside = "other:53 { hosts { 192.0.2.1 other } }\n"
        original = (
            outside
            + ".:53 {\n    # vm-hosts begin\n    hosts {\n        10.0.0.1 carbide-api.forge\n        fd00::99 carbide-api.forge\n        10.0.0.2 carbide-pxe.forge carbide-static-pxe.forge\n        fallthrough\n    }\n    # vm-hosts end\n}\n"
        )
        result = ipv6.dns_config(original, self.addresses)
        self.assertTrue(result.startswith(outside))
        self.assertIn("10.0.0.1 carbide-api.forge", result)
        self.assertIn("10.0.0.2 carbide-pxe.forge carbide-static-pxe.forge", result)
        self.assertNotIn("fd00::99", result)
        self.assertIn("fd00::10 carbide-api.forge", result)
        self.assertIn("fd00::20 carbide-pxe.forge carbide-static-pxe.forge", result)
        self.assertEqual(ipv6.dns_config(result, self.addresses), result)
        for text in (outside, original + original):
            with self.subTest(text=text), self.assertRaises(ValueError):
                ipv6.dns_config(text, self.addresses)

    def test_dhcp_updates_only_nico_provisioning_options(self):
        hook = {"library": "/opt/nico/libdhcp.so", "parameters": {"unrelated": "keep"}}
        config = {
            "Dhcp6": {
                "hooks-libraries": [{"library": "/other.so", "parameters": {}}, hook],
                "subnet6": [],
            }
        }
        original = copy.deepcopy(config)
        result = ipv6.dhcp_config(config, self.addresses)
        self.assertEqual(
            result["Dhcp6"]["hooks-libraries"][1]["parameters"],
            {
                "unrelated": "keep",
                "hook-dns-servers-ipv6": "fd00::53",
                "hook-provisioning-server-ipv6": "fd00::20",
            },
        )
        self.assertEqual(config, original)
        self.assertEqual(
            result["Dhcp6"]["hooks-libraries"][0],
            original["Dhcp6"]["hooks-libraries"][0],
        )
        for hooks in ([], [hook, hook]):
            with self.subTest(hooks=hooks), self.assertRaises(ValueError):
                ipv6.dhcp_config({"Dhcp6": {"hooks-libraries": hooks}}, self.addresses)

    def test_wrong_cluster_is_rejected_before_any_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "inventory.json").write_text("{}")
            (root / "cluster-uid").write_text("old-cluster")
            replies = [
                "kind-test",
                "test-control-plane",
                json.dumps({"metadata": {"uid": "new-cluster"}}),
            ]
            with patch.dict(
                os.environ, {"LOCAL_DEV_VM_HOSTS_DIR": str(root)}
            ), patch.object(
                sys, "argv", ["configure_ipv6.py", "--output", str(root / "output")]
            ), patch.object(
                ipv6.lab, "run", side_effect=replies
            ) as run:
                with self.assertRaisesRegex(ValueError, "different cluster"):
                    ipv6.main()
                self.assertEqual(run.call_count, 3)
                self.assertFalse((root / "output").exists())

    def test_new_route_is_recorded_before_install_and_existing_routes_are_not_adopted(
        self,
    ):
        with tempfile.TemporaryDirectory() as directory:
            state_path = Path(directory) / "network-state.json"
            state_path.write_text(json.dumps({"routes": []}))
            owned = ["6", "fd00::10/128", "fd00::1"]

            def install(*args):
                if args[0] == "ip":
                    return "[]"
                self.assertIn(owned, json.loads(state_path.read_text())["routes"])
                return ""

            with patch.object(ipv6.lab, "run", side_effect=install):
                ipv6.add_route(state_path, owned[1], owned[2])
            self.assertEqual(json.loads(state_path.read_text())["routes"], [owned])
            with patch.object(
                ipv6.lab, "run", return_value='[{"gateway":"fd00::1"}]'
            ) as run:
                ipv6.add_route(state_path, "fd00::20/128", owned[2])
                self.assertEqual(run.call_count, 1)
            self.assertEqual(json.loads(state_path.read_text())["routes"], [owned])

    def test_retry_reconciles_deployment_after_configmap_patch_succeeded(self):
        state = {"value": "old", "fail": True}

        def apply(*args, **kwargs):
            if "configmap" in args:
                state["value"] = "new"
            elif "deployment" in args and state["fail"]:
                state["fail"] = False
                raise subprocess.CalledProcessError(1, args)
            return ""

        with patch.object(ipv6.lab, "run", side_effect=apply) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                ipv6.apply_config("ns", "config", "dhcp", "key", state["value"], "new")
            self.assertEqual(state["value"], "new")
            ipv6.apply_config("ns", "config", "dhcp", "key", state["value"], "new")
            calls = [call.args for call in run.call_args_list]
            self.assertEqual(sum("configmap" in args for args in calls), 1)
            self.assertEqual(sum("deployment" in args for args in calls), 2)
            self.assertEqual(sum("rollout" in args for args in calls), 1)
            patch_body = json.loads(
                next(args[-1] for args in reversed(calls) if "deployment" in args)
            )
            self.assertIn(
                "devspace.nvidia.com/ipv6-provisioning-sha256",
                patch_body["spec"]["template"]["metadata"]["annotations"],
            )

    def test_dns_requires_exact_ipv6_membership_and_no_ipv4_resolver(self):
        for addresses, expected in (
            ("fd00::530", False),
            ("fd00:0:0::53", True),
            ("fd00::53 192.0.2.53", False),
        ):
            with self.subTest(addresses=addresses):
                self.assertEqual(
                    validator.dns_matches("Link 2 (eth0): " + addresses, "fd00::53"),
                    expected,
                )

    def test_validator_rejects_another_local_address_as_peer(self):
        arguments = [
            "validate_ipv6_only.py",
            "--address",
            "fd00::1",
            "--peer",
            "fd00::2",
            "--dns",
            "fd00::53",
            "--interface",
            "eth0",
        ]
        addresses = [
            {"ifname": "eth1", "addr_info": [{"family": "inet6", "local": "fd00::2"}]}
        ]
        result = subprocess.CompletedProcess([], 0, json.dumps(addresses), "")
        with patch.object(sys, "argv", arguments), patch.object(
            sys, "stderr", io.StringIO()
        ), patch.object(validator.subprocess, "run", return_value=result) as run:
            with self.assertRaises(SystemExit) as error:
                validator.main()
            self.assertEqual(error.exception.code, 2)
            self.assertEqual(run.call_count, 1)

    def test_validator_retains_results_when_cloud_init_status_is_unusable(self):
        arguments = [
            "validate_ipv6_only.py",
            "--address",
            "fd00::1",
            "--peer",
            "fd00::2",
            "--dns",
            "fd00::53",
            "--interface",
            "eth0",
        ]
        cases = [
            ("empty", 0, "", False, False, False),
            ("command failure", 1, "failed to read status", False, False, False),
            ("wrong JSON shape", 2, "[]", False, False, False),
            ("healthy", 0, '{"status":"done"}', True, True, True),
            (
                "reported error",
                1,
                '{"status":"error","errors":["failure"]}',
                False,
                False,
                True,
            ),
            (
                "reported warning",
                2,
                '{"status":"done","recoverable_errors":{"WARNING":["warning"]}}',
                True,
                True,
                False,
            ),
        ]
        for name, code, output, complete, no_errors, no_warnings in cases:
            with self.subTest(name=name):

                def execute(command, **kwargs):
                    result_code, stdout = 0, ""
                    if command == ["ip", "-j", "address"]:
                        stdout = json.dumps(
                            [
                                {
                                    "ifname": "eth0",
                                    "addr_info": [
                                        {"family": "inet6", "local": "fd00::1"}
                                    ],
                                }
                            ]
                        )
                    elif command[:3] == ["ip", "-j", "-4"]:
                        stdout = "[]"
                    elif command[:2] == ["resolvectl", "dns"]:
                        stdout = "Link 2 (eth0): fd00::53"
                    elif command[0] == "curl":
                        if "-4" in command:
                            result_code = 7
                        else:
                            stdout = "status=200 local=fd00::1 remote=2001:db8::1 ssl_verify=0"
                    elif command[:2] == ["cloud-init", "status"]:
                        result_code, stdout = code, output
                    return subprocess.CompletedProcess(command, result_code, stdout, "")

                captured = io.StringIO()
                with patch.object(sys, "argv", arguments), patch.object(
                    sys, "stdout", captured
                ), patch.object(
                    validator.subprocess, "run", side_effect=execute
                ), patch.object(
                    validator.socket, "create_connection"
                ) as connection:
                    connection.return_value.__enter__.return_value.recv.return_value = (
                        b"SSH-2.0-test"
                    )
                    with self.assertRaises(SystemExit) as result:
                        validator.main()
                self.assertEqual(result.exception.code, 0 if name == "healthy" else 1)
                rows = json.loads(captured.getvalue())
                checks = {row["check"]: row for row in rows}
                self.assertTrue(checks["internet-ipv6"]["passed"])
                self.assertTrue(checks["peer-ipv6-ssh"]["passed"])
                self.assertEqual(checks["cloud-init"]["stdout"], output)
                self.assertEqual(checks["cloud-init"]["returncode"], code)
                self.assertEqual(checks["cloud-init"]["passed"], complete)
                if name in ("empty", "command failure", "wrong JSON shape"):
                    self.assertEqual(
                        checks["cloud-init"]["error"],
                        "cloud-init status did not return a JSON object",
                    )
                else:
                    self.assertNotIn("error", checks["cloud-init"])
                self.assertEqual(checks["no-cloud-init-errors"]["passed"], no_errors)
                self.assertEqual(
                    checks["no-unreviewed-cloud-init-warnings"]["passed"], no_warnings
                )
                self.assertTrue(checks["cloud-init-schema"]["passed"])

    def test_guest_validator_rejects_ipv4_and_self_peer_before_running_checks(self):
        script = Path(__file__).with_name("validate_ipv6_only.py")
        for own, peer in (("192.0.2.1", "fd00::2"), ("fd00::1", "fd00::1")):
            with self.subTest(own=own, peer=peer):
                result = subprocess.run(
                    [
                        sys.executable,
                        str(script),
                        "--address",
                        own,
                        "--peer",
                        peer,
                        "--dns",
                        "fd00::53",
                        "--interface",
                        "eth0",
                    ],
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
