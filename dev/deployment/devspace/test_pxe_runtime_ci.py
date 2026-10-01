# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Keep PXE runtime coverage connected to the image that CI already builds."""

import unittest
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[3]


class PxeRuntimeCiTests(unittest.TestCase):
    def test_workflow_edits_exercise_kustomize(self):
        ci = yaml.safe_load((ROOT / ".github/workflows/ci.yaml").read_text())
        filters = next(
            step["with"]["filters"]
            for step in ci["jobs"]["prepare"]["steps"]
            if step.get("uses", "").startswith("dorny/paths-filter@")
        )
        self.assertIn(
            ".github/workflows/ci.yaml", yaml.safe_load(filters)["pxe_kustomize"]
        )

    def test_release_image_runs_pxe_runtime_regression(self):
        ci = yaml.safe_load((ROOT / ".github/workflows/ci.yaml").read_text())
        release = ci["jobs"]["build-release-container-x86_64"]
        self.assertTrue(release["with"].get("test_pxe_boot_artifacts"))
        workflow = yaml.safe_load(
            (ROOT / ".github/workflows/docker-build.yml").read_text()
        )
        steps = workflow["jobs"]["build"]["steps"]
        load = next(step for step in steps if step.get("id") == "loadgate")
        self.assertIn(
            "(inputs.test_pxe_boot_artifacts && !inputs.push)", load["env"]["NEEDED"]
        )
        build = next(step for step in steps if step.get("id") == "build")
        self.assertIn(
            "steps.loadgate.outputs.load != 'true'", build["with"]["provenance"]
        )
        pull = next(
            step for step in steps
            if step.get("run") == 'docker pull "$PXE_TEST_IMAGE"'
        )
        self.assertEqual(
            pull["if"],
            "${{ inputs.test_pxe_boot_artifacts && steps.loadgate.outputs.load != 'true' }}",
        )
        self.assertEqual(
            pull["env"]["PXE_TEST_IMAGE"], "${{ steps.tag_list.outputs.primary }}"
        )
        test = next(step for step in steps if step.get("id") == "pxe-runtime")
        self.assertEqual(test["if"], "${{ inputs.test_pxe_boot_artifacts }}")
        self.assertEqual(
            test["env"]["PXE_TEST_IMAGE"], "${{ steps.tag_list.outputs.primary }}"
        )
        self.assertEqual(test["run"], "bash .github/ci/test_pxe_runtime.sh")
        self.assertLess(steps.index(build), steps.index(pull))
        self.assertLess(steps.index(pull), steps.index(test))


if __name__ == "__main__":
    unittest.main()
