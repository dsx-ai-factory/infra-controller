# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Exercise CI compiler selection through the workflow's actual shell step."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[3]
BUILD_CONTAINER = "example.test/compiler:revision"


class CiDevImagesTest(unittest.TestCase):
    def test_compiler_publication_decision_matches_both_producers(self):
        jobs = yaml.safe_load((ROOT / ".github/workflows/ci.yaml").read_text())["jobs"]
        self.assertEqual(
            {jobs["test-dev-dhcp-image"]["steps"][-1]["env"]["PUBLISH_COMPILER"]},
            {jobs[f"build-container-{compiler}"]["with"]["push"]
             for compiler in ("x86_64", "aarch64")},
        )

    def run_build(self, arch="amd64", platform="linux/x86_64", rebuilt=True,
                  published=False, fail_build=False):
        workflow = yaml.safe_load((ROOT / ".github/workflows/ci.yaml").read_text())
        step = workflow["jobs"]["test-dev-dhcp-image"]["steps"][-1]
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            log = directory / "docker.jsonl"
            docker = directory / "docker"
            docker.write_text(
                f"#!{sys.executable}\n"
                "import json, os, sys\n"
                "args = sys.argv[1:]\n"
                "with open(os.environ['DOCKER_TEST_LOG'], 'a') as stream:\n"
                "    stream.write(json.dumps(args) + '\\n')\n"
                "if args == ['context', 'show']:\n"
                "    print('native-remote')\n"
                "    sys.exit(0)\n"
                "if any(os.environ.get(key) != 'native-remote' for key in ('DOCKER_CONTEXT', 'BUILDX_BUILDER')):\n"
                "    sys.exit('Docker context and builder must use the captured context')\n"
                "if args[0] == 'info':\n"
                "    print(os.environ['DOCKER_TEST_PLATFORM'])\n"
                "elif args[0] == 'pull' and os.environ['DOCKER_TEST_UNPUBLISHED'] == 'true':\n"
                "    sys.exit('compiler was not published')\n"
                "elif args[0] == 'image':\n"
                "    print(os.environ['ARCH'])\n"
                "elif args[0] == 'build' and os.environ['DOCKER_TEST_FAIL_BUILD'] == 'true':\n"
                "    sys.exit('compiler build failed')\n"
                "elif args[0] == 'run':\n"
                "    sys.stdin.read()\n"
            )
            docker.chmod(0o755)
            env = dict(os.environ)
            env.update({key: value for key, value in step["env"].items()
                        if "${{" not in value})
            env.update(
                PATH=f"{directory}{os.pathsep}{env['PATH']}",
                ARCH=arch,
                BUILDX_BUILDER="unrelated-remote-builder",
                BUILD_CONTAINER=BUILD_CONTAINER,
                BUILD_CONTAINER_REBUILT=str(rebuilt).lower(),
                PUBLISH_COMPILER=str(published).lower(),
                KEA_VERSION=(ROOT / "dev/docker/kea.version").read_text().strip(),
                DOCKER_TEST_LOG=str(log),
                DOCKER_TEST_PLATFORM=platform,
                DOCKER_TEST_UNPUBLISHED=str(rebuilt and not published).lower(),
                DOCKER_TEST_FAIL_BUILD=str(fail_build).lower(),
            )
            result = subprocess.run(
                ["bash", "-c", step["run"]], cwd=ROOT, env=env,
                capture_output=True, text=True, timeout=30,
            )
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            return result, calls

    def test_unpublished_compiler_is_built_locally_before_runtime_and_service(self):
        for arch, platform, compiler in (
            ("amd64", "linux/x86_64", "x86_64"),
            ("arm64", "linux/aarch64", "aarch64"),
        ):
            with self.subTest(arch=arch):
                result, calls = self.run_build(arch=arch, platform=platform)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual([call[0] for call in calls],
                                 ["context", "info", "build", "image", "build", "build", "image", "run"])
                compiler_build = calls[2]
                self.assertEqual(compiler_build[compiler_build.index("--file") + 1],
                                 f"dev/docker/Dockerfile.build-container-{compiler}")
                self.assertEqual(compiler_build[compiler_build.index("--platform") + 1],
                                 f"linux/{arch}")
                self.assertEqual(compiler_build[compiler_build.index("--tag") + 1], BUILD_CONTAINER)
                self.assertIn(f"KEA_VERSION={(ROOT / 'dev/docker/kea.version').read_text().strip()}",
                              compiler_build)
                self.assertIn(f"CONTAINER_BUILD={BUILD_CONTAINER}", calls[5])

    def test_published_or_reused_compiler_is_pulled(self):
        for rebuilt, published in ((True, True), (False, False)):
            with self.subTest(rebuilt=rebuilt, published=published):
                result, calls = self.run_build(rebuilt=rebuilt, published=published)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual([call[0] for call in calls],
                                 ["context", "info", "pull", "image", "build", "build", "image", "run"])
                self.assertEqual(calls[2], ["pull", "--platform", "linux/amd64", BUILD_CONTAINER])

    def test_compiler_build_failure_stops_runtime_and_smoke(self):
        result, calls = self.run_build(fail_build=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("compiler build failed", result.stderr)
        self.assertEqual([call[0] for call in calls], ["context", "info", "build"])

    def test_mismatched_daemon_stops_before_pulling_or_building(self):
        result, calls = self.run_build(arch="arm64")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Docker daemon platform linux/x86_64 does not match requested linux/arm64",
                      result.stderr)
        self.assertEqual([call[0] for call in calls], ["context", "info"])


if __name__ == "__main__":
    unittest.main()
