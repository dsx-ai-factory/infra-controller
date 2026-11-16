# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Execute the rendered nic-init command against a stub ip, without touching the kernel."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import yaml


CHART = Path(__file__).resolve().parents[1]
FIXTURE = CHART / "tests" / "values.yaml"

# Answers for the stub, keyed by the ip subcommand. A scenario overrides what
# it needs; everything else succeeds silently.
DEFAULTS = {
    "link show": {"rc": 0},
    "addr add": {"rc": 0},
    "addr show": {"out": "inet6 2001:db8::251/127 scope global"},
    "route replace": {"rc": 0},
    "rule list": {"out": "0:\tfrom all lookup local"},
    "rule add": {"rc": 0},
}

STUB = f"""#!{sys.executable}
import json, os, sys
argv = sys.argv[1:]
if argv[:2] == ["-f", "inet6"]:
    family, argv = "inet6", argv[2:]
else:
    family = "inet"
key = " ".join(argv[:2])
with open(os.environ["STUB_LOG"], "a") as log:
    log.write(json.dumps({{"family": family, "args": argv}}) + "\\n")
answers = json.loads(os.environ["STUB_ANSWERS"])
calls = int(open(os.environ["STUB_LOG"]).read().count("\\n"))
answer = answers.get(key + " " + family, answers.get(key, {{}}))
# A list answers successive calls of that command; the last entry repeats.
if isinstance(answer, list):
    index = sum(1 for line in open(os.environ["STUB_LOG"]) if json.loads(line)["args"][:2] == argv[:2]) - 1
    answer = answer[min(index, len(answer) - 1)]
if answer.get("out"):
    print(answer["out"])
if answer.get("err"):
    print(answer["err"], file=sys.stderr)
sys.exit(answer.get("rc", 0))
"""


def rendered_nic_init():
    rendered = subprocess.run(
        ["helm", "template", "unbound-test", str(CHART), "-f", str(FIXTURE),
         "--show-only", "templates/daemonset.yaml"],
        capture_output=True, text=True, timeout=30, check=True,
    )
    pod = yaml.safe_load(rendered.stdout)["spec"]["template"]["spec"]
    return next(c for c in pod["initContainers"] if c["name"] == "nic-init")


class NicInitCommandTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.container = rendered_nic_init()
        cls.env = {item["name"]: item["value"] for item in cls.container["env"]}

    def run_script(self, answers):
        """Run the rendered script under busybox sh with ip and sleep replaced."""
        self.assertEqual(self.container["command"][:2], ["/busybox/sh", "-ec"])
        with tempfile.TemporaryDirectory() as directory:
            stub = Path(directory) / "ip-stub"
            stub.write_text(STUB)
            stub.chmod(0o755)
            log = Path(directory) / "calls.jsonl"
            log.touch()
            # Shell functions win over busybox's own applets, so the script's
            # unchanged `ip` and `sleep` calls reach the stub.
            prelude = f'ip() {{ "{stub}" "$@"; }}\nsleep() {{ :; }}\n'
            process = subprocess.run(
                ["busybox", "sh", "-ec", prelude + self.container["command"][2]],
                env={**os.environ, **self.env, "STUB_LOG": str(log),
                     "STUB_ANSWERS": json.dumps({**DEFAULTS, **answers})},
                capture_output=True, text=True, timeout=30,
            )
            calls = [json.loads(line) for line in log.read_text().splitlines()]
        return process, calls

    @staticmethod
    def commands(calls):
        return [(c["family"], " ".join(c["args"][:2])) for c in calls]

    def test_first_run_numbers_the_interface_and_routes_replies(self):
        """Addresses, link, DAD wait, one default route and one rule per family, in order."""
        process, calls = self.run_script({
            "addr show": [{"out": "inet6 2001:db8::251/127 scope global tentative"},
                          {"out": "inet6 2001:db8::251/127 scope global"}],
        })
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(self.commands(calls), [
            ("inet", "link show"),
            ("inet", "addr add"), ("inet6", "addr add"),
            ("inet", "link set"),
            ("inet6", "addr show"), ("inet6", "addr show"),
            ("inet", "route replace"), ("inet6", "route replace"),
            ("inet", "rule list"), ("inet", "rule add"),
            ("inet6", "rule list"), ("inet6", "rule add"),
        ])
        # Every address, route and rule carries its own family's rendered value.
        by_command = {(c["family"], " ".join(c["args"][:2])): c["args"] for c in calls}
        expected = {
            ("inet", "addr add"): ["addr", "add", "198.51.100.251/31", "dev", "svc0"],
            ("inet6", "addr add"): ["addr", "add", "2001:db8::251/127", "dev", "svc0"],
            ("inet", "route replace"): ["route", "replace", "default", "via", "198.51.100.250",
                                       "dev", "svc0", "table", "100"],
            ("inet6", "route replace"): ["route", "replace", "default", "via", "2001:db8::250",
                                        "dev", "svc0", "table", "100"],
            ("inet", "rule add"): ["rule", "add", "from", "198.51.100.251/32", "table", "100"],
            ("inet6", "rule add"): ["rule", "add", "from", "2001:db8::251/128", "table", "100"],
        }
        self.assertEqual({key: by_command[key] for key in expected}, expected)
        self.assertIn("svc0 holds 198.51.100.251/31 and 2001:db8::251/127", process.stdout)

    def test_second_run_in_the_same_namespace_changes_nothing(self):
        """A restarted init container tolerates present addresses and adds no second rule."""
        process, calls = self.run_script({
            "addr add": {"rc": 2, "err": "ip: RTNETLINK answers: File exists"},
            "rule list inet": {"out": "32765:\tfrom 198.51.100.251 lookup 100"},
            "rule list inet6": {"out": "32765:\tfrom 2001:db8::251 lookup 100"},
        })
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(process.stdout.count("already present"), 2)
        self.assertNotIn("rule add", [cmd for _, cmd in self.commands(calls)])
        self.assertEqual([cmd for _, cmd in self.commands(calls)].count("route replace"), 2)

    def test_any_other_address_failure_stops_before_the_link_comes_up(self):
        process, calls = self.run_script({
            "addr add inet6": {"rc": 2, "err": "ip: RTNETLINK answers: Permission denied"},
        })
        self.assertEqual(process.returncode, 1)
        self.assertIn("Permission denied", process.stderr)
        self.assertNotIn(("inet", "link set"), self.commands(calls))

    def test_missing_interface_gives_up_after_fifteen_probes(self):
        process, calls = self.run_script({"link show": {"rc": 1}})
        self.assertEqual(process.returncode, 1)
        self.assertIn("svc0 never appeared", process.stderr)
        self.assertEqual(self.commands(calls), [("inet", "link show")] * 15)

    def test_duplicate_address_is_reported_as_such_on_the_first_poll(self):
        """Linux keeps tentative set beside dadfailed; the failure must win."""
        process, calls = self.run_script({
            "addr show": {"out": "inet6 2001:db8::251/127 scope global tentative dadfailed"},
        })
        self.assertEqual(process.returncode, 1)
        self.assertIn("is a duplicate", process.stderr)
        self.assertEqual([cmd for _, cmd in self.commands(calls)].count("addr show"), 1)

    def test_address_that_stays_tentative_times_out(self):
        process, calls = self.run_script({
            "addr show": {"out": "inet6 2001:db8::251/127 scope global tentative"},
        })
        self.assertEqual(process.returncode, 1)
        self.assertIn("stayed tentative", process.stderr)
        self.assertEqual([cmd for _, cmd in self.commands(calls)].count("addr show"), 10)

    def test_route_failure_names_its_gateway(self):
        process, calls = self.run_script({
            "route replace inet6": {"rc": 2, "err": "ip: RTNETLINK answers: Nexthop has invalid gateway"},
        })
        self.assertEqual(process.returncode, 1)
        self.assertIn("default route via 2001:db8::250 failed", process.stderr)
        self.assertNotIn("rule list", [cmd for _, cmd in self.commands(calls)])


if __name__ == "__main__":
    unittest.main()
