# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Exercise the local development image entrypoint without a Docker daemon."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]


class DevImagesTest(unittest.TestCase):
    def run_make(self, platform="linux/aarch64", arguments=(), fail_command=""):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            log = directory / "docker.jsonl"
            docker = directory / "docker"
            docker.write_text(
                f"#!{sys.executable}\n"
                "import json, os, sys\n"
                "with open(os.environ['DOCKER_TEST_LOG'], 'a') as stream:\n"
                "    stream.write(json.dumps(sys.argv[1:]) + '\\n')\n"
                "if sys.argv[1] == os.environ.get('DOCKER_TEST_FAIL'):\n"
                "    sys.exit(7)\n"
                "if sys.argv[1:] == ['context', 'show']:\n"
                "    print('native-remote')\n"
                "    sys.exit(0)\n"
                "if any(os.environ.get(key) != 'native-remote' for key in ('DOCKER_CONTEXT', 'BUILDX_BUILDER')):\n"
                "    sys.exit('Docker context and builder must use the captured context')\n"
                "if sys.argv[1] == 'info':\n"
                "    print(os.environ['DOCKER_TEST_PLATFORM'])\n"
            )
            docker.chmod(0o755)
            env = dict(os.environ)
            env.update(
                PATH=f"{directory}{os.pathsep}{env['PATH']}",
                DOCKER_TEST_LOG=str(log),
                DOCKER_TEST_PLATFORM=platform,
                DOCKER_TEST_FAIL=fail_command,
                DOCKER_DEFAULT_PLATFORM="linux/ppc64le",
                DOCKER_CONTEXT="native-remote",
                BUILDX_BUILDER="unrelated-remote-builder",
            )
            result = subprocess.run(
                ["make", "--no-print-directory", "images-dev", *arguments],
                cwd=ROOT, env=env, capture_output=True, text=True, timeout=30,
            )
            calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
            return result, calls

    def test_daemon_platform_selects_native_compiler_and_local_service(self):
        for platform, arch, compiler in (
            ("linux/aarch64", "arm64", "aarch64"),
            ("linux/arm64", "arm64", "aarch64"),
            ("linux/x86_64", "amd64", "x86_64"),
            ("linux/amd64", "amd64", "x86_64"),
        ):
            with self.subTest(platform=platform):
                result, calls = self.run_make(platform)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(calls[0], ["context", "show"])
                self.assertEqual(calls[1], ["info", "--format", "{{.OSType}}/{{.Architecture}}"])
                builds = calls[2:]
                self.assertEqual(len(builds), 3)
                self.assertEqual(
                    [call[call.index("--file") + 1] for call in builds],
                    [f"dev/docker/Dockerfile.build-container-{compiler}",
                     "dev/docker/Dockerfile.dev-runtime", "dev/docker/Dockerfile.dev-services"],
                )
                for call in builds:
                    self.assertEqual(call[0], "build")
                    self.assertEqual(call[call.index("--platform") + 1], f"linux/{arch}")
                    self.assertEqual(call[-1], ".")
                    self.assertNotIn("--push", call)
                self.assertEqual(builds[-1][builds[-1].index("--target") + 1], "api")
                self.assertEqual(builds[-1][builds[-1].index("--tag") + 1], "nico-dev-api:latest")
                version = (ROOT / "dev/docker/kea.version").read_text().strip()
                for call in (builds[0], builds[-1]):
                    self.assertIn(f"KEA_VERSION={version}", call)

    def test_selected_target_and_base_images_are_propagated(self):
        result, calls = self.run_make(arguments=(
            "DEV_TARGET=dhcp", "DEV_IMAGE=example/dhcp:check",
            "DEV_BUILD_CONTAINER=example/build:check", "DEV_RUNTIME_CONTAINER=example/runtime:check",
        ))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("example/build:check", calls[2])
        self.assertIn("example/runtime:check", calls[3])
        service = calls[-1]
        self.assertEqual(service[service.index("--target") + 1], "dhcp")
        self.assertEqual(service[service.index("--tag") + 1], "example/dhcp:check")
        self.assertIn("CONTAINER_BUILD=example/build:check", service)
        self.assertIn("CONTAINER_RUNTIME=example/runtime:check", service)

    def test_unsupported_platforms_stop_before_building(self):
        for platform in ("linux/ppc64le", "windows/amd64", ""):
            with self.subTest(platform=platform):
                result, calls = self.run_make(platform)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("requires a Linux AMD64 or ARM64 Docker daemon", result.stderr)
                self.assertEqual(len(calls), 2)

    def test_unknown_target_is_rejected_before_docker(self):
        for target in ("not-a-service", "", "%", "api dhcp", "api bad"):
            with self.subTest(target=target):
                result, calls = self.run_make(arguments=(f"DEV_TARGET={target}",))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Unsupported DEV_TARGET", result.stderr)
                self.assertEqual(calls, [])

    def test_docker_failure_stops_dependent_builds(self):
        for command, count in (("context", 1), ("info", 2), ("build", 3)):
            with self.subTest(command=command):
                result, calls = self.run_make(fail_command=command)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(calls), count)


if __name__ == "__main__":
    unittest.main()
