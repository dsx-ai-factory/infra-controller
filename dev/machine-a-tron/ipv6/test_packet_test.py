# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
import copy
import datetime
import json
import os
import signal
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import packet_test


def snapshot():
    return {
        "schema_version": 1,
        "updated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "instance_id": "00812118-8c3e-44cb-8dc5-fd9250ddc8f8",
        "network_virtualization_type": 5,
        "tenant_interfaces": [
            {
                "function_type": 0,
                "is_l2_segment": False,
                "has_network_security_group": False,
                "vpc_vni": 42,
                "addresses": [
                    {
                        "address_family": 1,
                        "ip": "10.84.100.1",
                        "prefix": "10.84.100.0/31",
                        "gateway": "10.84.100.0/31",
                        "interface_prefix": "10.84.100.1/32",
                    },
                    {
                        "address_family": 2,
                        "ip": "fd00:84:100::1",
                        "prefix": "fd00:84:100::/127",
                        "interface_prefix": "fd00:84:100::1/128",
                    },
                ],
            }
        ],
    }


class SnapshotTests(unittest.TestCase):
    def test_accepts_canonical_addresses_in_either_order(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "snapshot.json"
            data = snapshot()
            data["tenant_interfaces"][0]["addresses"].reverse()
            path.write_text(json.dumps(data))
            self.assertEqual(packet_test.load_snapshot(path), data)

    def test_rejects_unmodeled_topologies_and_stale_snapshots(self):
        changes = [
            ("stale", lambda d: d.update(updated_at="2000-01-01T00:00:00+00:00")),
            (
                "NSG",
                lambda d: d["tenant_interfaces"][0].update(
                    has_network_security_group=True
                ),
            ),
            ("VF", lambda d: d["tenant_interfaces"][0].update(function_type=1)),
            (
                "SLAAC",
                lambda d: d["tenant_interfaces"][0]["addresses"][1].update(
                    ip="",
                    prefix="fd00:84:100::/64",
                    interface_prefix="fd00:84:100::/64",
                ),
            ),
            ("IPv4 only", lambda d: d["tenant_interfaces"][0]["addresses"].pop()),
            (
                "gateway mismatch",
                lambda d: d["tenant_interfaces"][0]["addresses"][0].update(
                    gateway="10.84.100.2/31"
                ),
            ),
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "snapshot.json"
            for name, change in changes:
                with self.subTest(name=name):
                    data = snapshot()
                    change(data)
                    path.write_text(json.dumps(data))
                    with self.assertRaises(ValueError):
                        packet_test.load_snapshot(path)

    def test_pair_rejects_different_vpcs_and_overlapping_linknets(self):
        first = snapshot()
        second = copy.deepcopy(first)
        second["instance_id"] = "2d6e9beb-1b7a-4c9d-aa35-abe5d589b314"
        with self.assertRaisesRegex(ValueError, "overlap"):
            packet_test.validate_pair([first, second])
        second["tenant_interfaces"][0]["vpc_vni"] += 1
        with self.assertRaisesRegex(ValueError, "same VPC"):
            packet_test.validate_pair([first, second])

    def test_refresh_is_not_a_configuration_change(self):
        original = snapshot()
        refreshed = copy.deepcopy(original)
        refreshed["updated_at"] = "2000-01-01T00:00:00+00:00"
        self.assertEqual(
            packet_test.fingerprint(original), packet_test.fingerprint(refreshed)
        )
        refreshed["tenant_interfaces"][0]["addresses"][1]["ip"] = "fd00:84:100::3"
        self.assertNotEqual(
            packet_test.fingerprint(original), packet_test.fingerprint(refreshed)
        )

    def test_interrupted_mutation_remains_registered_for_cleanup(self):
        lab = packet_test.Lab(Path("/unused"))
        with patch.object(packet_test, "run", side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                lab.change(["create", "owned-resource"], ["delete", "owned-resource"])
        self.assertEqual(lab.undo, [["delete", "owned-resource"]])

    def test_signal_during_launch_tracks_and_terminates_the_child(self):
        with tempfile.TemporaryDirectory() as directory:
            lab = packet_test.Lab(Path(directory))
            original_popen = subprocess.Popen
            children = []

            def start_and_interrupt(*args, **kwargs):
                child = original_popen(*args, **kwargs)
                children.append(child)
                os.kill(os.getpid(), signal.SIGTERM)
                return child

            def interrupted(signum, frame):
                raise KeyboardInterrupt

            previous = signal.signal(signal.SIGTERM, interrupted)
            try:
                with patch.object(packet_test.subprocess, "Popen", start_and_interrupt):
                    with self.assertRaises(KeyboardInterrupt):
                        lab.launch(
                            [sys.executable, "-c", "import time; time.sleep(60)"],
                            "child.log",
                        )
                self.assertEqual(lab.processes, children)
                self.assertEqual(lab.cleanup(), [])
                self.assertEqual(children[0].returncode, -signal.SIGTERM)
            finally:
                signal.signal(signal.SIGTERM, previous)
                for child in children:
                    if child.poll() is None:
                        child.kill()
                        child.wait()
                for stream in lab.logs:
                    stream.close()

    def test_router_lifetime_update_never_exposes_an_empty_file(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "lifetime"
            path.write_text("30")
            observations = []
            original_open = Path.open

            def open_and_observe(candidate, mode="r", *args, **kwargs):
                stream = original_open(candidate, mode, *args, **kwargs)
                if "w" in mode:
                    with original_open(path) as reader:
                        observations.append(reader.read())
                return stream

            with patch.object(Path, "open", open_and_observe):
                packet_test.write_lifetime(path, 0)
            self.assertEqual(observations, ["30"])
            self.assertEqual(path.read_text(), "0")

    def test_cleanup_continues_after_one_failure(self):
        lab = packet_test.Lab(Path("/unused"))
        lab.undo = [["delete", "first"], ["delete", "second"]]
        with patch.object(
            packet_test, "run", side_effect=[RuntimeError("failed"), ""]
        ) as execute:
            self.assertEqual(lab.cleanup(), ["failed"])
            self.assertEqual(execute.call_count, 2)
            self.assertEqual(execute.call_args.args, ("delete", "first"))


if __name__ == "__main__":
    unittest.main()
