# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Keep PXE runtime coverage connected to the image that CI already builds."""

import os
from pathlib import Path
import subprocess
import unittest

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

    def test_non_publishing_release_image_runs_pxe_runtime_regression(self):
        ci = yaml.safe_load((ROOT / ".github/workflows/ci.yaml").read_text())
        release = ci["jobs"]["build-release-container-x86_64"]
        self.assertEqual(
            release["with"]["test_pxe_boot_artifacts"],
            "${{ needs.prepare.outputs.publish_images != 'true' }}",
        )
        self.assertEqual(
            release["with"]["push"],
            "${{ needs.prepare.outputs.publish_images == 'true' }}",
        )
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
        self.assertEqual(
            build["with"]["tags"],
            "${{ inputs.test_pxe_boot_artifacts && steps.loadgate.outputs.load != 'true' "
            "&& format('{0}:pxe-test-{1}-{2}', inputs.image_name, github.run_id, "
            "github.run_attempt) || steps.tag_list.outputs.tags }}",
        )
        pull = next(step for step in steps if step.get("id") == "pxe-pull")
        self.assertEqual(
            pull["if"],
            "${{ inputs.test_pxe_boot_artifacts && steps.loadgate.outputs.load != 'true' }}",
        )
        self.assertEqual(
            pull["env"]["PXE_TEST_IMAGE"],
            "${{ inputs.image_name }}@${{ steps.build.outputs.digest }}",
        )
        test = next(step for step in steps if step.get("id") == "pxe-runtime")
        self.assertEqual(test["if"], "${{ inputs.test_pxe_boot_artifacts }}")
        self.assertEqual(
            test["env"]["PXE_TEST_IMAGE"],
            "${{ steps.loadgate.outputs.load == 'true' && steps.tag_list.outputs.primary "
            "|| format('{0}@{1}', inputs.image_name, steps.build.outputs.digest) }}",
        )
        self.assertEqual(test["run"], "bash .github/ci/test_pxe_runtime.sh")
        self.assertLess(steps.index(build), steps.index(pull))
        self.assertLess(steps.index(pull), steps.index(test))
        promote = next(step for step in steps if step.get("id") == "pxe-promote")
        self.assertLess(steps.index(test), steps.index(promote))
        self.assertEqual(
            promote["if"],
            "${{ inputs.test_pxe_boot_artifacts && steps.loadgate.outputs.load != 'true' "
            "&& steps.pxe-runtime.outcome == 'success' }}",
        )
        self.assertEqual(promote["env"]["SOURCE"], pull["env"]["PXE_TEST_IMAGE"])
        self.assertEqual(promote["env"]["TAGS"], "${{ steps.tag_list.outputs.tags }}")
        result = subprocess.run(
            ["bash", "-c", 'docker() { printf "<%s>\\n" "$@"; }\n' + promote["run"]],
            env={**os.environ, "SOURCE": "registry/image@sha256:fixture",
                 "TAGS": "registry/image:version\n\nregistry/image:latest\n"},
            capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout.splitlines(),
            ["<buildx>", "<imagetools>", "<create>", "<--prefer-index=false>",
             "<--tag>", "<registry/image:version>", "<--tag>", "<registry/image:latest>",
             "<registry/image@sha256:fixture>"],
        )


if __name__ == "__main__":
    unittest.main()
