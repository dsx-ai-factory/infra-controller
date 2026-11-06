# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Cover compiler fallback and cache policies not exercised by canonical PR CI."""

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
    def test_cache_archives_share_identity_and_only_save_from_main(self):
        jobs = yaml.safe_load((ROOT / ".github/workflows/ci.yaml").read_text())["jobs"]
        steps = jobs["test-dev-dhcp-image"]["steps"]
        by_id = {step["id"]: step for step in steps if "id" in step}
        restore = by_id["sccache"]
        lookup = by_id["sccache-saved"]
        export = by_id["sccache-export"]
        save = next(step for step in steps if step.get("uses") == "actions/cache/save@v4")
        # Actions includes the path in the cache identity, not just the key.
        for step in (lookup, save):
            self.assertEqual(step["with"]["path"], restore["with"]["path"])
            self.assertEqual(step["with"]["key"], "${{ steps.sccache.outputs.cache-primary-key }}")
        self.assertEqual(by_id["dev-build"]["env"]["SCCACHE_SEED_DIR"], export["env"]["CACHE_EXPORT_DIR"])
        self.assertIn(restore["with"]["path"].replace("~", "${HOME}"), by_id["sccache-dir"]["run"])
        # Saving is gated through lookup and export, which PR runs skip.
        self.assertEqual(lookup["if"],
                         "github.ref == 'refs/heads/main' && steps.sccache.outputs.cache-hit != 'true'")
        self.assertEqual(export["if"],
                         "steps.sccache-saved.outcome == 'success' && steps.sccache-saved.outputs.cache-hit != 'true'")
        self.assertEqual(save["if"], "steps.sccache-export.outcome == 'success'")

    def run_unpublished_build(self, arch):
        jobs = yaml.safe_load((ROOT / ".github/workflows/ci.yaml").read_text())["jobs"]
        step = next(step for step in jobs["test-dev-dhcp-image"]["steps"]
                    if step.get("id") == "dev-build")
        self.assertEqual(
            {step["env"]["PUBLISH_COMPILER"]},
            {jobs[f"build-container-{compiler}"]["with"]["push"]
             for compiler in ("x86_64", "aarch64")},
        )
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
                "elif args[0] == 'info':\n"
                "    print('linux/' + os.environ['ARCH'])\n"
                "elif args[0] == 'pull':\n"
                "    sys.exit('compiler was not published')\n"
                "elif args[:2] == ['image', 'inspect']:\n"
                "    print(os.environ['ARCH'])\n"
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
                GITHUB_OUTPUT=str(output),
                SCCACHE_SEED_DIR=str(directory / "seed"),
                GITHUB_RUN_ID="8157",
                GITHUB_RUN_ATTEMPT="1",
                BUILD_CONTAINER=BUILD_CONTAINER,
                BUILD_CONTAINER_REBUILT="true",
                PUBLISH_COMPILER="false",
                KEA_VERSION=(ROOT / "dev/docker/kea.version").read_text().strip(),
                DOCKER_TEST_LOG=str(log),
            )
            result = subprocess.run(
                ["bash", "-c", step["run"]], cwd=ROOT, env=env,
                capture_output=True, text=True, timeout=30,
            )
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            return result, calls

    def test_unpublished_compiler_is_built_and_used_for_service(self):
        for arch, compiler in (("amd64", "x86_64"), ("arm64", "aarch64")):
            with self.subTest(arch=arch):
                result, calls = self.run_unpublished_build(arch)
                self.assertEqual(result.returncode, 0, result.stderr)
                compiler_build = next(call for call in calls if call[0] == "build"
                                      and f"dev/docker/Dockerfile.build-container-{compiler}" in call)
                service_build = next(call for call in calls if call[0] == "build"
                                     and "dev/docker/Dockerfile.dev-services" in call)
                compiler_image = compiler_build[compiler_build.index("--tag") + 1]
                self.assertNotEqual(compiler_image, BUILD_CONTAINER)
                self.assertIn(f"CONTAINER_BUILD={compiler_image}", service_build)


if __name__ == "__main__":
    unittest.main()
