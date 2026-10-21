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
        self.assertEqual(set(events), {"pull_request", "push", "workflow_dispatch"})
        self.assertEqual(events["push"]["branches"], ["pull-request/[0-9]+"])
        self.assertNotIn("tags", events["push"])
        self.assertEqual(events["pull_request"]["paths"], events["push"]["paths"])
        for path in ("devspace.yaml", "dev/deployment/devspace/**",
                     ".github/workflows/devspace-tests.yml", ".github/ci/test_devspace_workflow.py"):
            self.assertIn(path, events["pull_request"]["paths"])
        self.assertEqual(workflow["permissions"], {"contents": "read"})

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
