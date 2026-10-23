# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "setup-kind.sh"


class SetupKindTest(unittest.TestCase):
    def run_setup(self, args=(), default_config=False, **settings):
        with tempfile.TemporaryDirectory(prefix="kind setup ") as temporary:
            root = Path(temporary)
            home = root / "home"
            output = home / ".kube/kind-fixture.yaml" if default_config else root / "selected config"
            if not default_config:
                output.write_text("original config\n")
            log = root / "calls.jsonl"
            command = root / "command"
            command.write_text(f"#!{sys.executable}\n" + r'''
import json, os, sys
from pathlib import Path
name, args = Path(sys.argv[0]).name, sys.argv[1:]
record = {"command": name, "args": args}
if name == "kind" and args[:2] == ["create", "cluster"]:
    record["config"] = json.loads(Path(args[args.index("--config") + 1]).read_text())
with open(os.environ["TEST_LOG"], "a") as log:
    log.write(json.dumps(record) + "\n")
if os.environ.get("TEST_FAIL") == " ".join([name, *args[:2]]):
    sys.exit(73)
if name == "uname":
    print(os.environ.get("TEST_OS", "Linux") if args == ["-s"] else os.environ.get("TEST_HOST_ARCH", "x86_64"))
elif name == "docker":
    if args == ["context", "show"]:
        print(os.environ.get("TEST_CONTEXT", "selected"))
    elif args[:2] == ["context", "inspect"]:
        print(os.environ.get("TEST_ENDPOINT", "unix:///var/run/docker.sock"))
    else:
        assert os.environ["DOCKER_CONTEXT"] == os.environ.get("TEST_CONTEXT", "selected")
        assert os.environ["KIND_EXPERIMENTAL_PROVIDER"] == "docker"
        if args[0] == "info":
            print(os.environ.get("TEST_DAEMON", "linux/x86_64"))
elif name == "kind":
    if args == ["version"]:
        print(os.environ.get("TEST_KIND_VERSION", "kind v0.32.0 go1.26.3 linux/amd64"))
    else:
        assert os.environ["DOCKER_CONTEXT"] == os.environ.get("TEST_CONTEXT", "selected")
        assert os.environ["KIND_EXPERIMENTAL_PROVIDER"] == "docker"
        if args[:2] == ["get", "clusters"]:
            print("fixture" if os.environ.get("TEST_EXISTING") == "true" else "")
        elif args[:2] == ["get", "nodes"]:
            print(os.environ.get("TEST_NODES", "fixture-control-plane"))
        elif args[:2] == ["export", "kubeconfig"]:
            Path(args[args.index("--kubeconfig") + 1]).write_text("selected cluster\n")
elif name == "kubectl":
    assert args[args.index("--context") + 1] == "kind-fixture"
    assert Path(args[args.index("--kubeconfig") + 1]).is_file()
    assert args[args.index("--kubeconfig") + 1] != os.environ["TEST_OUTPUT"]
    if "get" in args:
        print(json.dumps({"spec": {"podCIDRs": json.loads(os.environ.get("TEST_CIDRS", '["10.244.0.0/24"]'))},
                          "status": {"nodeInfo": {"architecture": os.environ.get("TEST_NODE_ARCH", "amd64")}}}))
else:
    sys.exit("unexpected command")
''')
            command.chmod(0o755)
            for name in ("uname", "docker", "kind", "kubectl"):
                (root / name).symlink_to(command)
            environment = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}",
                               HOME=str(home), TMPDIR=str(root), TEST_LOG=str(log), TEST_OUTPUT=str(output),
                               KUBECONFIG=str(root / "unrelated config"),
                               DOCKER_CONTEXT="unrelated", KIND_EXPERIMENTAL_PROVIDER="podman")
            environment.update(settings)
            output_args = [] if default_config else ["--kubeconfig", str(output)]
            result = subprocess.run(
                ["bash", str(SCRIPT), "--name", "fixture", *output_args, *args],
                env=environment, capture_output=True, text=True, timeout=20,
            )
            calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
            for call in calls:
                if call["command"] == "kind" and call["args"][:2] == ["create", "cluster"]:
                    self.assertFalse(Path(call["args"][call["args"].index("--config") + 1]).exists())
            self.assertEqual(list(root.glob("nico-kind.*")), [])
            return result, calls, output.read_text() if output.exists() else None

    def test_create_native_cluster_and_export_only_after_validation(self):
        cases = [
            ("x86_64", "linux/x86_64", "amd64", "ipv4", ["10.244.0.0/24"]),
            ("aarch64", "linux/aarch64", "arm64", "dual", ["10.244.0.0/24", "fd00:10:244::/64"]),
        ]
        for host, daemon, arch, family, cidrs in cases:
            with self.subTest(arch=arch):
                result, calls, output = self.run_setup(
                    ["--ip-family", family], TEST_HOST_ARCH=host, TEST_DAEMON=daemon,
                    TEST_NODE_ARCH=arch, TEST_CIDRS=json.dumps(cidrs),
                    TEST_CONTEXT="default", DOCKER_HOST="unix:///tmp/native.sock",
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                pull = next(call for call in calls if call["command"] == "docker" and call["args"][0] == "pull")
                self.assertEqual(pull["args"][1:3], ["--platform", f"linux/{arch}"])
                self.assertIn("@sha256:", pull["args"][-1])
                create = next(call for call in calls if call["args"][:2] == ["create", "cluster"])
                self.assertEqual(create["config"]["networking"], {"ipFamily": family, "apiServerAddress": "127.0.0.1"})
                self.assertNotIn("--retain", create["args"])
                self.assertEqual(calls[-1]["args"][:2], ["export", "kubeconfig"])
                self.assertEqual(output, "selected cluster\n")

    def test_macos_uses_local_docker_runtime_architecture(self):
        # An Intel/Rosetta CLI can use an ARM64 Docker Desktop daemon.
        for daemon, arch in (("linux/aarch64", "arm64"), ("linux/x86_64", "amd64")):
            with self.subTest(arch=arch):
                result, calls, output = self.run_setup(
                    TEST_OS="Darwin", TEST_HOST_ARCH="x86_64", TEST_DAEMON=daemon,
                    TEST_NODE_ARCH=arch, TEST_ENDPOINT="unix:///Users/dev/.docker/run/docker.sock",
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                pull = next(call for call in calls if call["command"] == "docker" and call["args"][0] == "pull")
                self.assertEqual(pull["args"][1:3], ["--platform", f"linux/{arch}"])
                self.assertEqual(output, "selected cluster\n")

    def test_default_kubeconfig_creates_missing_home_directory(self):
        result, calls, output = self.run_setup(default_config=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(calls[-1]["args"][-1].endswith("/home/.kube/kind-fixture.yaml"))
        self.assertEqual(output, "selected cluster\n")

    def test_help_does_not_contact_host_or_write_config(self):
        result, calls, output = self.run_setup(["--help"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Usage: setup-kind.sh", result.stdout)
        self.assertEqual(calls, [])
        self.assertEqual(output, "original config\n")

    def test_reuse_does_not_pull_recreate_or_change_unrelated_context(self):
        result, calls, output = self.run_setup(["--image", "different-node:tag"], TEST_EXISTING="true")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(call["args"][0] in ("pull", "create", "delete") for call in calls))
        self.assertEqual(output, "selected cluster\n")

    def test_incompatible_existing_cluster_preserves_output_and_cluster(self):
        cases = [
            ({"TEST_CIDRS": '["fd00:10:244::/64"]'}, "uses ipv6, requested ipv4"),
            ({"TEST_CIDRS": "[]"}, "invalid pod CIDRs"),
            ({"TEST_CIDRS": '["10.244.0.0/24", "10.244.1.0/24"]'}, "invalid pod CIDRs"),
            ({"TEST_NODE_ARCH": "arm64"}, "node architecture arm64"),
            ({"TEST_NODES": "fixture-control-plane\nfixture-worker"}, "one control-plane node"),
        ]
        for settings, message in cases:
            with self.subTest(message=message):
                result, calls, output = self.run_setup(TEST_EXISTING="true", **settings)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertEqual(output, "original config\n")
                self.assertFalse(any(call["args"][0] in ("pull", "create", "delete") for call in calls))

    def test_preflight_rejects_remote_or_non_native_daemon(self):
        for settings in ({"TEST_ENDPOINT": "ssh://another-host"},
                         {"TEST_CONTEXT": "default", "DOCKER_HOST": "ssh://another-host"},
                         {"TEST_KIND_VERSION": "kind v0.31.0 go1.26.0 linux/amd64"},
                         {"TEST_DAEMON": "linux/aarch64"},
                         {"TEST_OS": "FreeBSD"}):
            with self.subTest(settings=settings):
                result, calls, output = self.run_setup(**settings)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(output, "original config\n")
                self.assertFalse(any(call["args"][:2] == ["get", "clusters"] for call in calls))

    def test_creation_and_discovery_failures_do_not_export_or_continue(self):
        for failure in ("kind get clusters", "kind create cluster"):
            with self.subTest(failure=failure):
                result, calls, output = self.run_setup(TEST_FAIL=failure)
                self.assertEqual(result.returncode, 73, result.stderr)
                self.assertEqual(output, "original config\n")
                self.assertFalse(any(call["command"] == "kubectl" for call in calls))
                self.assertFalse(any(call["args"][:2] == ["export", "kubeconfig"] for call in calls))

    def test_invalid_options_fail_before_host_commands(self):
        for args in (["--ip-family", "bad"], ["--name", "../elsewhere"], ["--name"], ["--kubeconfig", ""]):
            with self.subTest(args=args):
                result, calls, output = self.run_setup(args)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(calls, [])
                self.assertEqual(output, "original config\n")


if __name__ == "__main__":
    unittest.main()
