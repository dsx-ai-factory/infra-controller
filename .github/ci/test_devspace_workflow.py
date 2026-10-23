#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Protect the advisory DevSpace workflow's trigger and failure boundaries."""

from pathlib import Path
import unittest

import yaml


WORKFLOW = Path(__file__).resolve().parents[1] / "workflows/devspace-tests.yml"


class DevSpaceWorkflowTests(unittest.TestCase):
    def test_only_pr_changes_and_manual_runs(self):
        workflow = yaml.safe_load(WORKFLOW.read_text())
        # PyYAML's YAML 1.1 loader interprets the Actions key `on` as True.
        events = workflow[True]
        self.assertEqual(set(events), {"pull_request_target", "push", "workflow_dispatch"})
        self.assertEqual(events["push"]["branches"], ["pull-request/[0-9]+"])
        self.assertNotIn("tags", events["push"])
        self.assertEqual(events["pull_request_target"]["paths"], events["push"]["paths"])
        for path in ("devspace.yaml", "dev/deployment/devspace/**",
                     "pxe/scout-firmware-scripts/nvidia/dgxh100/cx7/metadata.toml",
                     ".github/workflows/devspace-tests.yml", ".github/ci/test_devspace_workflow.py"):
            self.assertIn(path, events["pull_request_target"]["paths"])
        self.assertEqual(workflow["permissions"], {"contents": "read"})

    def test_job_rejects_forks_before_checkout(self):
        workflow = yaml.safe_load(WORKFLOW.read_text())
        condition = workflow["jobs"]["devspace-tests"]["if"]
        self.assertEqual(
            " ".join(condition.split()),
            "github.event_name == 'workflow_dispatch' "
            "|| (github.event_name == 'push' && startsWith(github.ref, 'refs/heads/pull-request/')) "
            "|| (github.event_name == 'pull_request_target' "
            "&& github.event.pull_request.head.repo.full_name == github.repository)")

    def test_checkout_pins_event_commit(self):
        workflow = yaml.safe_load(WORKFLOW.read_text())
        checkout = workflow["jobs"]["devspace-tests"]["steps"][0]
        self.assertTrue(checkout["uses"].startswith("actions/checkout@"))
        self.assertEqual(
            checkout["with"].get("ref"),
            "${{ github.event_name == 'pull_request_target' "
            "&& github.event.pull_request.head.sha || github.sha }}")
        self.assertFalse(checkout["with"]["persist-credentials"])

    def test_test_failures_remain_visible_and_isolated(self):
        workflow = yaml.safe_load(WORKFLOW.read_text())
        self.assertEqual(set(workflow["jobs"]), {"devspace-tests"})
        job = workflow["jobs"]["devspace-tests"]
        self.assertNotIn("needs", job)
        self.assertNotIn("continue-on-error", job)
        self.assertLessEqual(job["timeout-minutes"], 10)
        for step in job["steps"]:
            self.assertNotIn("continue-on-error", step)
        commands = [step["run"] for step in job["steps"] if "run" in step]
        self.assertIn(
            "/usr/bin/python3 -B -m unittest discover "
            "-s dev/deployment/devspace/tests -t dev/deployment/devspace -v", commands)
        self.assertIn("/usr/bin/python3 -B .github/ci/test_devspace_workflow.py", commands)


if __name__ == "__main__":
    unittest.main()
