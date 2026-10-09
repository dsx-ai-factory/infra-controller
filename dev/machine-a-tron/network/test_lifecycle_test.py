# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
import unittest
import argparse
import ipaddress
import json
import os
import signal
import subprocess
import sys
from pathlib import Path
import tempfile
from types import SimpleNamespace
from unittest.mock import Mock, patch

import lifecycle_test


class LifecycleTests(unittest.TestCase):
    def runner(self):
        args = SimpleNamespace(machine_id=["host-a", "host-b"], tenant_org="test")
        runner = lifecycle_test.Lifecycle(args)
        runner.rpc = Mock()
        return runner

    def test_ipv6_prefix_rejects_saturated_capacity_counters(self):
        self.assertEqual(lifecycle_test.ipv6_prefix("fd42:84::/64").prefixlen, 64)
        with self.assertRaisesRegex(argparse.ArgumentTypeError, "capacity counters"):
            lifecycle_test.ipv6_prefix("fd42:84::/63")

    def test_release_requires_both_core_and_mat_to_forget_instances(self):
        runner = self.runner()
        for core, mat, expected in [
            ([{"id": {"value": "old"}}], [], False),
            ([], [{"instance_id": "old"}], False),
            ([], [], True),
        ]:
            with self.subTest(core=core, mat=mat):
                runner.rpc.return_value = {"instances": core}
                with patch.object(
                    lifecycle_test.packet_test, "fetch_networks", return_value=mat
                ):
                    self.assertEqual(runner.released(["old"]), expected)

    def test_preflight_rejects_busy_or_non_mat_targets_without_mutating(self):
        runner = self.runner()
        machines = [
            {"machine_id": name, "device_kind": "machine"}
            for name in runner.args.machine_id
        ]
        for mat, instances in [
            (machines[:1], []),
            (machines, [{"machineId": {"id": "host-a"}}]),
        ]:
            with self.subTest(mat=mat, instances=instances):
                runner.rpc.side_effect = [{"instances": instances}]
                with patch.object(
                    lifecycle_test.packet_test,
                    "mat_request",
                    return_value={"machines": mat},
                ):
                    with self.assertRaises(ValueError):
                        runner.preflight()
                self.assertTrue(
                    all(c.args[0].startswith("Find") for c in runner.rpc.call_args_list)
                )
                runner.rpc.reset_mock()

    def test_cleanup_checks_ownership_after_an_unknown_create_outcome(self):
        runner = self.runner()
        owned = {
            "id": {"value": "ours"},
            "metadata": runner.metadata("instance"),
            "status": {"tenant": {"state": "READY"}},
        }
        foreign = {"id": {"value": "foreign"}, "metadata": {"labels": []}}
        for instance, released in [(owned, True), (foreign, False)]:
            with self.subTest(released=released):
                runner.rpc.side_effect = [{"instances": [instance]}, {}]
                if released:
                    runner.release_instances([instance["id"]["value"]])
                    self.assertEqual(runner.rpc.call_args.args[0], "ReleaseInstance")
                else:
                    with self.assertRaisesRegex(ValueError, "ownership"):
                        runner.release_instances([instance["id"]["value"]])
                    self.assertEqual(runner.rpc.call_count, 1)
                runner.rpc.reset_mock()

    def test_failed_allocation_reply_cleans_owned_resources_and_waits_for_drain(self):
        runner = self.runner()
        runner.args.ipv4_prefix = ipaddress.ip_network("10.84.200.0/29")
        runner.args.ipv6_prefix = ipaddress.ip_network("fd42:84:200::/120")
        runner.args.timeout = 10
        runner.preflight = Mock()
        runner.capacity_restored = Mock(return_value=True)
        objects = {}
        draining = []
        calls = []

        def rpc(method, request):
            calls.append((method, request))
            if method in ("CreateVpc", "CreateVpcPrefix"):
                objects[request["id"]["value"]] = request
            elif method == "AllocateInstances":
                for instance in request["instanceRequests"]:
                    objects[instance["instanceId"]["value"]] = {
                        "id": instance["instanceId"],
                        "metadata": instance["metadata"],
                    }
                raise RuntimeError("allocation reply lost")
            elif method == "FindInstancesByIds":
                return {
                    "instances": [
                        objects[i["value"]]
                        for i in request["instanceIds"]
                        if i["value"] in objects
                    ]
                }
            elif method == "GetVpcPrefixes":
                if request["deleted"] == "DELETED_FILTER_INCLUDE" and draining:
                    return {"vpcPrefixes": [draining.pop()]}
                return {
                    "vpcPrefixes": [
                        objects[i] for i in runner.prefix_ids if i in objects
                    ]
                }
            elif method == "FindVpcsByIds":
                return {
                    "vpcs": [objects[runner.vpc_id]] if runner.vpc_id in objects else []
                }
            elif method in ("ReleaseInstance", "DeleteVpcPrefix", "DeleteVpc"):
                resource = objects.pop(request["id"]["value"])
                if method == "DeleteVpcPrefix":
                    draining.append(resource)
                if method == "DeleteVpc":
                    self.assertFalse(draining)
            else:
                self.fail(method)
            return {}

        runner.rpc.side_effect = rpc
        with tempfile.TemporaryDirectory() as temp:
            runner.args.output = Path(temp) / "run"
            with patch.object(
                lifecycle_test.packet_test, "fetch_networks", return_value=[]
            ), patch.object(lifecycle_test.time, "sleep"):
                with self.assertRaisesRegex(RuntimeError, "allocation reply lost"):
                    runner.execute()
            result = json.loads((runner.args.output / "result.json").read_text())
        self.assertEqual(result["result"], "FAIL")
        self.assertEqual(result["cleanup_errors"], [])
        self.assertEqual(len(result["instance_ids"]), 2)
        self.assertFalse(objects)
        self.assertEqual(sum(m == "ReleaseInstance" for m, _ in calls), 2)

    def test_signal_during_packet_launch_terminates_child_before_returning(self):
        runner = self.runner()
        runner.args.mat_url = "http://mat.invalid"
        runner.args.dhcp_server = Path("/unused")
        runner.args.internet_url = "https://example.invalid"
        runner.args.dns_upstream = "127.0.0.53"
        runner.args.mat_ca = None
        runner.args.mat_insecure = False
        runner.args.nat44_interface = None
        runner.args.nat66_interface = None
        children = []
        original_popen = subprocess.Popen

        def start_and_interrupt(*args, **kwargs):
            child = original_popen(
                [sys.executable, "-c", "import time; time.sleep(60)"], **kwargs
            )
            children.append(child)
            os.kill(os.getpid(), signal.SIGTERM)
            return child

        def interrupted(signum, frame):
            raise KeyboardInterrupt

        previous = signal.signal(signal.SIGTERM, interrupted)
        try:
            with tempfile.TemporaryDirectory() as directory:
                runner.args.output = Path(directory)
                with patch.object(
                    lifecycle_test.subprocess, "Popen", start_and_interrupt
                ):
                    with self.assertRaises(KeyboardInterrupt):
                        runner.packet_check(["one", "two"], 1)
                self.assertEqual(children[0].poll(), -signal.SIGTERM)
        finally:
            signal.signal(signal.SIGTERM, previous)
            for child in children:
                if child.poll() is None:
                    child.kill()
                    child.wait()

    def test_second_generation_requests_the_released_addresses(self):
        runner = self.runner()
        runner.prefix_ids = ["prefix-v4", "prefix-v6"]
        addresses = [
            {1: "10.84.200.1", 2: "fd00:84:200::1"},
            {1: "10.84.200.3", 2: "fd00:84:200::3"},
        ]
        requests = runner.allocation_requests(["new-a", "new-b"], addresses)
        for index, request in enumerate(requests):
            interface = request["config"]["network"]["interfaces"][0]
            self.assertEqual(request["machineId"]["id"], runner.args.machine_id[index])
            self.assertEqual(interface["ipAddress"], addresses[index][1])
            self.assertEqual(
                interface["ipv6InterfaceConfig"]["ipAddress"], addresses[index][2]
            )
            self.assertEqual(request["instanceId"]["value"], ["new-a", "new-b"][index])


if __name__ == "__main__":
    unittest.main()
