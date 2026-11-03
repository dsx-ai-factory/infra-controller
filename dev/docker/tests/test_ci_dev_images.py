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
BUILD_STEP = "Build and smoke-test native development DHCP image"


class CiDevImagesTest(unittest.TestCase):
    def test_compiler_publication_decision_matches_both_producers(self):
        jobs = yaml.safe_load((ROOT / ".github/workflows/ci.yaml").read_text())["jobs"]
        self.assertEqual(
            {next(step for step in jobs["test-dev-dhcp-image"]["steps"]
                  if step["name"] == BUILD_STEP)["env"]["PUBLISH_COMPILER"]},
            {jobs[f"build-container-{compiler}"]["with"]["push"]
             for compiler in ("x86_64", "aarch64")},
        )

    def test_cache_archive_policy_matches_shared_ci(self):
        jobs = yaml.safe_load((ROOT / ".github/workflows/ci.yaml").read_text())["jobs"]
        steps = jobs["test-dev-dhcp-image"]["steps"]
        by_id = {step["id"]: step for step in steps if "id" in step}
        restore = by_id["sccache"]
        self.assertEqual(restore["with"]["key"],
                         "sccache-dev-dhcp-${{ matrix.arch }}-${{ hashFiles('Cargo.lock') }}")
        self.assertEqual(restore["with"]["restore-keys"], "sccache-dev-dhcp-${{ matrix.arch }}-")
        self.assertEqual(restore["with"]["path"], "~/.cache/sccache-dev-dhcp")
        lookup = by_id["sccache-saved"]
        self.assertEqual(lookup["if"],
                         "github.ref == 'refs/heads/main' && steps.sccache.outputs.cache-hit != 'true'")
        self.assertTrue(lookup["with"]["lookup-only"])
        export = by_id["sccache-export"]
        self.assertEqual(export["if"],
                         "steps.sccache-saved.outcome == 'success' && steps.sccache-saved.outputs.cache-hit != 'true'")
        save = next(step for step in steps if step.get("uses") == "actions/cache/save@v4")
        self.assertEqual(save["if"], "steps.sccache-export.outcome == 'success'")
        self.assertEqual(save["with"]["key"], "${{ steps.sccache.outputs.cache-primary-key }}")
        self.assertEqual(restore["with"]["path"], save["with"]["path"])
        self.assertEqual(restore["with"]["path"], lookup["with"]["path"])
        self.assertEqual(by_id["dev-build"]["env"]["SCCACHE_SEED_DIR"], export["env"]["CACHE_EXPORT_DIR"])
        self.assertIn(restore["with"]["path"].replace("~", "${HOME}"), by_id["sccache-dir"]["run"])
        for step in (lookup, export, save):
            self.assertTrue(step["continue-on-error"])
        cleanup = steps[-1]
        self.assertEqual(cleanup["if"], "always() && steps.dev-build.outputs.compiler_image != ''")
        self.assertIn("test-dev-dhcp-image", jobs["prune-sccache"]["needs"])

    def run_build(self, arch="amd64", platform="linux/x86_64", rebuilt=True,
                  published=False, fail_build=False, fail_smoke=False, attempt="1", export_cache=False):
        workflow = yaml.safe_load((ROOT / ".github/workflows/ci.yaml").read_text())
        step = next(step for step in workflow["jobs"]["test-dev-dhcp-image"]["steps"]
                    if step["name"] == BUILD_STEP)
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            log = directory / "docker.jsonl"
            output = directory / "outputs"
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
                "elif args[:2] == ['image', 'inspect']:\n"
                "    print(os.environ['ARCH'])\n"
                "elif args[:2] == ['image', 'rm'] and os.environ['DOCKER_TEST_FAIL_SMOKE'] == 'true':\n"
                "    sys.exit('cleanup failed')\n"
                "elif args[0] == 'build' and os.environ['DOCKER_TEST_FAIL_BUILD'] == 'true':\n"
                "    sys.exit('compiler build failed')\n"
                "elif args[0] == 'run':\n"
                "    sys.stdin.read()\n"
                "    if os.environ['DOCKER_TEST_FAIL_SMOKE'] == 'true': sys.exit(9)\n"
            )
            docker.chmod(0o755)
            env = dict(os.environ)
            env.update({key: value for key, value in step["env"].items()
                        if "${{" not in value})
            env.update(
                PATH=f"{directory}{os.pathsep}{env['PATH']}",
                ARCH=arch,
                GITHUB_OUTPUT=str(output),
                SCCACHE_SEED_DIR=str(directory / "seed"),
                GITHUB_RUN_ID="8157",
                GITHUB_RUN_ATTEMPT=attempt,
                BUILDX_BUILDER="unrelated-remote-builder",
                BUILD_CONTAINER=BUILD_CONTAINER,
                BUILD_CONTAINER_REBUILT=str(rebuilt).lower(),
                PUBLISH_COMPILER=str(published).lower(),
                KEA_VERSION=(ROOT / "dev/docker/kea.version").read_text().strip(),
                DOCKER_TEST_LOG=str(log),
                DOCKER_TEST_PLATFORM=platform,
                DOCKER_TEST_UNPUBLISHED=str(rebuilt and not published).lower(),
                DOCKER_TEST_FAIL_BUILD=str(fail_build).lower(),
                DOCKER_TEST_FAIL_SMOKE=str(fail_smoke).lower(),
            )
            result = subprocess.run(
                ["bash", "-c", step["run"]], cwd=ROOT, env=env,
                capture_output=True, text=True, timeout=30,
            )
            if output.exists():
                outputs = dict(line.split("=", 1) for line in output.read_text().splitlines())
                env.update(DOCKER_CONTEXT=outputs["docker_context"],
                           BUILDX_BUILDER=outputs["docker_context"])
                steps = workflow["jobs"]["test-dev-dhcp-image"]["steps"]
                if result.returncode == 0 and export_cache:
                    export = next(step for step in steps if step.get("id") == "sccache-export")
                    env.update(BUILD_CONTAINER=outputs["compiler_image"],
                               CACHE_EXPORT_DIR=str(directory / "export"))
                    subprocess.run(["bash", "-c", export["run"]], cwd=ROOT, env=env,
                                   capture_output=True, text=True, timeout=30, check=True)
                cleanup = next(step for step in steps if step["name"] == "Remove development DHCP job images")
                cleaned = subprocess.run(["bash", "-c", cleanup["run"]], cwd=ROOT, env=env,
                                         capture_output=True, text=True, timeout=30)
                self.assertEqual(cleaned.returncode, 0, cleaned.stderr)
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
                                 ["context", "info", "build", "image", "build", "build", "image", "run", "image"])
                compiler_build = calls[2]
                self.assertEqual(compiler_build[compiler_build.index("--file") + 1],
                                 f"dev/docker/Dockerfile.build-container-{compiler}")
                self.assertEqual(compiler_build[compiler_build.index("--platform") + 1],
                                 f"linux/{arch}")
                self.assertEqual(compiler_build[compiler_build.index("--tag") + 1], f"nico-dev-build:ci-8157-1-{arch}")
                self.assertIn(f"KEA_VERSION={(ROOT / 'dev/docker/kea.version').read_text().strip()}",
                              compiler_build)
                self.assertIn(f"CONTAINER_BUILD=nico-dev-build:ci-8157-1-{arch}", calls[5])
                self.assertIn("CI_SCCACHE=true", calls[5])
                self.assertIn("--build-context", calls[5])
                self.assertEqual(calls[-1], [
                    "image", "rm", f"nico-dev-dhcp:ci-8157-1-{arch}",
                    f"nico-dev-runtime:ci-8157-1-{arch}", f"nico-dev-build:ci-8157-1-{arch}",
                ])

    def test_cache_export_keeps_compiler_until_scoped_cleanup(self):
        result, calls = self.run_build(export_cache=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        export = calls[-2]
        self.assertEqual(export[0], "build")
        self.assertNotIn("--no-cache", export)
        self.assertIn("SCCACHE_EXPORT_ID=8157-1", export)
        self.assertEqual(export[export.index("--target") + 1], "sccache-export")
        self.assertIn("CONTAINER_BUILD=nico-dev-build:ci-8157-1-amd64", export)
        self.assertTrue(export[export.index("--output") + 1].startswith("type=local,dest="))
        self.assertEqual(calls[-1][0:2], ["image", "rm"])

    def test_published_or_reused_compiler_is_pulled(self):
        for rebuilt, published in ((True, True), (False, False)):
            with self.subTest(rebuilt=rebuilt, published=published):
                result, calls = self.run_build(rebuilt=rebuilt, published=published)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual([call[0] for call in calls],
                                 ["context", "info", "pull", "image", "build", "build", "image", "run", "image"])
                self.assertEqual(calls[2], ["pull", "--platform", "linux/amd64", BUILD_CONTAINER])
                self.assertEqual(calls[-1], [
                    "image", "rm", "nico-dev-dhcp:ci-8157-1-amd64", "nico-dev-runtime:ci-8157-1-amd64",
                ])

    def test_compiler_build_failure_stops_runtime_and_smoke(self):
        result, calls = self.run_build(fail_build=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("compiler build failed", result.stderr)
        self.assertEqual([call[0] for call in calls], ["context", "info", "build", "image"])
        self.assertEqual(result.returncode, 1)
        self.assertEqual(calls[-1], [
            "image", "rm", "nico-dev-dhcp:ci-8157-1-amd64", "nico-dev-runtime:ci-8157-1-amd64",
            "nico-dev-build:ci-8157-1-amd64",
        ])

    def test_smoke_failure_survives_cleanup_failure_for_current_attempt(self):
        result, calls = self.run_build(rebuilt=False, fail_smoke=True, attempt="2")
        self.assertEqual(result.returncode, 9, result.stderr)
        self.assertIn("nico-dev-dhcp:ci-8157-2-amd64", calls[-2])
        self.assertEqual(calls[-1], [
            "image", "rm", "nico-dev-dhcp:ci-8157-2-amd64", "nico-dev-runtime:ci-8157-2-amd64",
        ])

    def test_mismatched_daemon_stops_before_pulling_or_building(self):
        result, calls = self.run_build(arch="arm64")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Docker daemon platform linux/x86_64 does not match requested linux/arm64",
                      result.stderr)
        self.assertEqual([call[0] for call in calls], ["context", "info"])


if __name__ == "__main__":
    unittest.main()
