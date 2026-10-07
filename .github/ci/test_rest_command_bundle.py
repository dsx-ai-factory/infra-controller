#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Keep the REST command manifest and bundle format honest at their boundaries."""

from __future__ import annotations

import copy
import gzip
import io
import json
import re
import shlex
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock


CI_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = CI_DIR.parents[1]
REST_API_DIR = REPOSITORY_ROOT / "rest-api"
sys.path.insert(0, str(CI_DIR))

import rest_command_bundle as bundle  # noqa: E402


class ManifestTest(unittest.TestCase):
    """``ManifestTest`` pins the complete source-controlled build contract."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.path = CI_DIR / "rest-command-manifest.json"
        cls.value = json.loads(cls.path.read_text())
        cls.manifest = bundle.load_source_manifest(cls.path, REST_API_DIR)

    def test_cli_defaults_follow_the_ci_helper_location(self) -> None:
        args = bundle.parser().parse_args(("check",))

        self.assertEqual(args.manifest, CI_DIR / "rest-command-manifest.json")
        self.assertEqual(args.repo_root, REST_API_DIR)

    def test_exact_build_contract(self) -> None:
        # Keep this inventory independent from `rest-command-manifest.json`.
        # Any package, target, CGO, or linker-flag change should require an
        # explicit test change that a reviewer can see.
        packages = {
            "api": "./api/cmd/api",
            "credsmgr": "./cert-manager/cmd/credsmgr",
            "flow": "./flow",
            "migrations": "./db/cmd/migrations",
            "mock-core": "./site-agent/cmd/mock-core",
            "mock-flow": "./site-agent/cmd/mock-flow",
            "nico-mcp": "./mcp/cmd/nico-mcp",
            "nicocli": "./cli/cmd/cli",
            "nsm": "./nvswitch-manager",
            "psm": "./powershelf-manager",
            "site-agent": "./site-agent/cmd/site-agent",
            "sitemgr": "./site-manager/cmd/sitemgr",
            "workflow": "./workflow/cmd/workflow",
        }
        static_stripped = ("-extldflags=-static", "-w", "-s")
        static_only = ("-extldflags=-static",)
        metadata_flags = {
            "api": (
                "-X=github.com/NVIDIA/infra-controller/rest-api/api/pkg/metadata.Version=${VERSION}",
                "-X=github.com/NVIDIA/infra-controller/rest-api/api/pkg/metadata.BuildTime=${BUILD_TIME_LEGACY}",
            ),
            "site-agent": (
                "-X=github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/metadata.Version=${VERSION}",
                "-X=github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/metadata.BuildTime=${BUILD_TIME_LEGACY}",
            ),
            "flow": (
                "-X=github.com/NVIDIA/infra-controller/rest-api/flow/pkg/metadata.Version=${VERSION}",
                "-X=github.com/NVIDIA/infra-controller/rest-api/flow/pkg/metadata.BuildTime=${BUILD_TIME_RFC3339}",
                "-X=github.com/NVIDIA/infra-controller/rest-api/flow/pkg/metadata.GitCommit=${SHORT_SHA}",
            ),
        }
        linux_flags = {
            "api": static_stripped + metadata_flags["api"],
            "migrations": static_stripped,
            "sitemgr": static_stripped,
            "workflow": static_stripped,
            "site-agent": static_stripped + metadata_flags["site-agent"],
            "mock-core": (),
            "mock-flow": (),
            "credsmgr": static_stripped,
            "flow": static_only + metadata_flags["flow"],
            "nicocli": static_stripped,
            "nico-mcp": static_stripped,
            "psm": static_only,
            "nsm": static_only,
        }
        common = set(packages) - {"nicocli", "nico-mcp"}
        inventories = {
            "linux-amd64": set(packages),
            "linux-arm64": set(packages),
            "darwin-arm64": common,
        }
        target_specs = {
            "linux-amd64": ("linux", "amd64"),
            "linux-arm64": ("linux", "arm64"),
            "darwin-arm64": ("darwin", "arm64"),
        }

        self.assertEqual(set(self.manifest.targets), set(target_specs))
        self.assertEqual(len(self.manifest.outputs), 37)
        for name, (goos, goarch) in target_specs.items():
            with self.subTest(target=name):
                target = self.manifest.targets[name]
                self.assertEqual((target.goos, target.goarch), (goos, goarch))
                actual = {
                    output.name
                    for output in self.manifest.outputs
                    if output.target == name
                }
                self.assertEqual(actual, inventories[name])

        for output in self.manifest.outputs:
            with self.subTest(target=output.target, name=output.name):
                self.assertEqual(output.package, packages[output.name])
                self.assertEqual(output.archive_path, f"bin/{output.name}")
                expected_cgo = (
                    output.target == "linux-amd64"
                    and output.name in {"mock-core", "mock-flow"}
                )
                self.assertEqual(output.cgo_enabled, expected_cgo)
                expected_flags = (
                    linux_flags[output.name]
                    if output.target.startswith("linux-")
                    else metadata_flags.get(output.name, ())
                )
                self.assertEqual(output.ldflags, expected_flags)

    def test_ldflags_keep_space_delimited_metadata_in_one_argument(self) -> None:
        output = self.output("linux-amd64", "api")
        values = {
            "BUILD_TIME_LEGACY": "2026-08-05 12:34:56",
            "BUILD_TIME_RFC3339": "2026-08-05T12:34:56Z",
            "SHORT_SHA": "1234567",
            "VERSION": "pull-request-4581-1234567",
        }
        resolved = bundle.resolve_ldflags(output, values)
        self.assertEqual(tuple(shlex.split(shlex.join(resolved))), resolved)
        self.assertIn(
            "-X=github.com/NVIDIA/infra-controller/rest-api/api/pkg/metadata.BuildTime=2026-08-05 12:34:56",
            resolved,
        )

    def test_space_separated_metadata_flag_is_rejected(self) -> None:
        with self.assertRaisesRegex(
            bundle.BundleError, "must use the -X=name=value form"
        ):
            bundle.metadata_values(("-X metadata.Version=1.2.3",))

    def test_invalid_manifest_cases(self) -> None:
        def set_unsupported_target(value: dict[str, Any]) -> None:
            targets = value["targets"]
            outputs = value["outputs"]
            target = next(
                target for target in targets if target["name"] == "linux-amd64"
            )
            target.update(name="linux-s390x", goarch="s390x")
            for output in outputs:
                if output["target"] == "linux-amd64":
                    output["target"] = "linux-s390x"

        cases = {
            "duplicate output": (
                lambda value: value["outputs"].append(
                    copy.deepcopy(value["outputs"][0])
                ),
                "duplicate target/name pair",
            ),
            "missing target": (
                lambda value: value["outputs"][0].__setitem__(
                    "target", "linux-s390x"
                ),
                "outputs[0].target is not declared: linux-s390x",
            ),
            "unsupported target": (
                set_unsupported_target,
                "is not supported: linux-s390x",
            ),
            "unsafe output": (
                lambda value: value["outputs"][0].__setitem__("output", "../api"),
                "outputs[0].output must be bin/api, got ../api",
            ),
            "trailing command hyphen": (
                lambda value: value["outputs"][0].update(
                    name="api-", output="bin/api-"
                ),
                "outputs[0].name is not a safe command name: api-",
            ),
            "unknown token": (
                lambda value: value["outputs"][0]["ldflags"].append(
                    "-X=example.Value=${UNKNOWN}"
                ),
                "outputs[0].ldflags uses unknown tokens",
            ),
            "space-separated metadata flag": (
                lambda value: value["outputs"][0]["ldflags"].append(
                    "-X metadata.Version=1.2.3"
                ),
                "outputs[0].ldflags must use the -X=name=value form",
            ),
            "boolean schema version": (
                lambda value: value.__setitem__("schema_version", True),
                "manifest schema_version must be integer 1, got True",
            ),
            "floating-point schema version": (
                lambda value: value.__setitem__("schema_version", 1.0),
                "manifest schema_version must be integer 1, got 1.0",
            ),
            "missing package": (
                lambda value: value["outputs"][0].__setitem__(
                    "package", "./not-a-package"
                ),
                "outputs[0].package does not exist: ./not-a-package",
            ),
            "absolute package suffix": (
                lambda value: value["outputs"][0].__setitem__("package", ".//tmp"),
                "outputs[0].package must be a normalized relative ./ path: .//tmp",
            ),
            "string cgo": (
                lambda value: value["outputs"][0].__setitem__("cgo_enabled", "0"),
                "outputs[0].cgo_enabled must be a boolean",
            ),
        }
        for name, (mutate, expected_error) in cases.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temp:
                value = copy.deepcopy(self.value)
                mutate(value)
                path = Path(temp) / "manifest.json"
                path.write_text(json.dumps(value))
                with self.assertRaisesRegex(
                    bundle.BundleError, re.escape(expected_error)
                ):
                    bundle.load_source_manifest(path, REST_API_DIR)

    def output(self, target: str, name: str) -> bundle.CommandBuild:
        return next(
            output
            for output in self.manifest.outputs
            if output.target == target and output.name == name
        )


class BundleFormatTest(unittest.TestCase):
    """``BundleFormatTest`` checks archive reproducibility and fail-closed reads."""

    def test_build_timestamp_is_canonical_and_fits_gzip(self) -> None:
        self.assertEqual(
            bundle.parse_build_timestamp("2026-08-05T12:34:56Z").isoformat(),
            "2026-08-05T12:34:56+00:00",
        )
        invalid = (
            "2026-8-5T1:2:3Z",
            "1969-12-31T23:59:59Z",
            "2106-02-07T06:28:16Z",
        )
        for timestamp in invalid:
            with self.subTest(timestamp=timestamp), self.assertRaises(
                bundle.BundleError
            ):
                bundle.parse_build_timestamp(timestamp)

    def test_file_description_accepts_known_static_wording(self) -> None:
        target = bundle.Target("linux-amd64", "linux", "amd64")
        command = bundle.CommandBuild(
            name="api",
            package="./api/cmd/api",
            target=target.name,
            cgo_enabled=False,
            ldflags=(),
            archive_path="bin/api",
        )
        cases = (
            "ELF 64-bit LSB executable, x86-64, statically linked",
            "ELF 64-bit LSB pie executable, x86-64, static-pie linked",
        )
        for file_description in cases:
            with self.subTest(file_description=file_description):
                bundle.validate_file_description(file_description, target, command)

    def test_file_description_rejects_wrong_linkage(self) -> None:
        target = bundle.Target("linux-arm64", "linux", "arm64")
        command = bundle.CommandBuild(
            name="api",
            package="./api/cmd/api",
            target=target.name,
            cgo_enabled=False,
            ldflags=(),
            archive_path="bin/api",
        )
        with self.assertRaises(bundle.BundleError):
            bundle.validate_file_description(
                "ELF 64-bit LSB executable, ARM aarch64, dynamically linked",
                target,
                command,
            )

    def test_file_description_accepts_darwin_arm64(self) -> None:
        target = bundle.Target("darwin-arm64", "darwin", "arm64")
        command = bundle.CommandBuild(
            name="api",
            package="./api/cmd/api",
            target=target.name,
            cgo_enabled=False,
            ldflags=(),
            archive_path="bin/api",
        )
        bundle.validate_file_description(
            "Mach-O 64-bit arm64 executable", target, command
        )

    def test_file_description_rejects_wrong_architecture(self) -> None:
        target = bundle.Target("darwin-arm64", "darwin", "arm64")
        command = bundle.CommandBuild(
            name="api",
            package="./api/cmd/api",
            target=target.name,
            cgo_enabled=False,
            ldflags=(),
            archive_path="bin/api",
        )
        with self.assertRaisesRegex(
            bundle.BundleError, re.escape("missing ['arm64']")
        ):
            bundle.validate_file_description(
                "Mach-O 64-bit x86_64 executable", target, command
            )

    def test_cgo_file_description_does_not_assume_linkage(self) -> None:
        target = bundle.Target("linux-amd64", "linux", "amd64")
        command = bundle.CommandBuild(
            name="mock-core",
            package="./site-agent/cmd/mock-core",
            target=target.name,
            cgo_enabled=True,
            ldflags=(),
            archive_path="bin/mock-core",
        )
        descriptions = (
            "ELF 64-bit LSB executable, x86-64, dynamically linked",
            "ELF 64-bit LSB executable, x86-64, statically linked",
        )
        for description in descriptions:
            with self.subTest(description=description):
                bundle.validate_file_description(description, target, command)

    def test_missing_external_tool_names_the_command(self) -> None:
        missing = FileNotFoundError(2, "No such file or directory", "file")
        with mock.patch.object(bundle.subprocess, "run", side_effect=missing):
            with self.assertRaisesRegex(bundle.BundleError, "cannot run file"):
                bundle.command_output(["file", "--version"], REST_API_DIR)

    def test_recorded_file_description_matches_reopened_binary(self) -> None:
        target = bundle.Target("linux-amd64", "linux", "amd64")
        command = bundle.CommandBuild(
            name="api",
            package="./api/cmd/api",
            target=target.name,
            cgo_enabled=False,
            ldflags=(),
            archive_path="bin/api",
        )
        recorded = {
            "file_description": "ELF 64-bit, x86-64, statically linked",
            "go_version": "go1.26.4",
            "vcs_modified": "unknown",
            "vcs_revision": "unavailable",
        }
        inspection = bundle.BinaryInspection(
            go_version="go1.26.4",
            vcs_modified="unknown",
            vcs_revision="unavailable",
            file_description=(
                "ELF 64-bit, x86-64, statically linked, stripped"
            ),
        )
        bundle.validate_file_description(
            recorded["file_description"], target, command
        )
        bundle.validate_file_description(
            inspection.file_description, target, command
        )
        with self.assertRaisesRegex(bundle.BundleError, "file_description"):
            bundle.validate_recorded_inspection("api", recorded, inspection)

    def test_bundle_is_deterministic_and_preserves_modes(self) -> None:
        manifest = b'{"schema_version": 1}\n'
        binaries = {"bin/api": b"api", "bin/workflow": b"workflow"}
        checksums = b"a" * 64 + b"  bin/api\n" + b"b" * 64 + b"  bin/workflow\n"
        timestamp = 1_786_000_000
        with tempfile.TemporaryDirectory() as temp:
            first = Path(temp) / "first.tar.gz"
            second = Path(temp) / "second.tar.gz"
            bundle.create_bundle(first, manifest, checksums, binaries, timestamp)
            bundle.create_bundle(second, manifest, checksums, binaries, timestamp)
            archive_bytes = first.read_bytes()
            self.assertEqual(archive_bytes, second.read_bytes())
            self.assertEqual(archive_bytes[:2], b"\x1f\x8b")
            self.assertEqual(archive_bytes[3] & 0x08, 0)
            self.assertEqual(int.from_bytes(archive_bytes[4:8], "little"), timestamp)
            self.assertEqual(archive_bytes[8], 2)

            with tarfile.open(first, mode="r:gz") as archive:
                members = archive.getmembers()
            self.assertEqual(
                [member.name for member in members],
                ["manifest.json", "SHA256SUMS", "bin/api", "bin/workflow"],
            )
            for member in members:
                with self.subTest(member=member.name):
                    self.assertEqual(member.uid, 0)
                    self.assertEqual(member.gid, 0)
                    self.assertEqual(member.uname, "")
                    self.assertEqual(member.gname, "")
                    self.assertEqual(member.mtime, timestamp)

            entries = bundle.read_bundle(first)
            self.assertEqual(entries["manifest.json"], (manifest, 0o644))
            self.assertEqual(entries["SHA256SUMS"], (checksums, 0o644))
            self.assertEqual(entries["bin/api"], (b"api", 0o755))
            self.assertEqual(entries["bin/workflow"], (b"workflow", 0o755))

    def test_publish_failure_removes_partial_output(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            output_dir = Path(temp) / "output"
            stage_dir = output_dir / "stage"
            stage_dir.mkdir(parents=True)
            manifest = stage_dir / "manifest.json"
            missing = stage_dir / "missing.sha256"
            archive = stage_dir / "bundle.tar.gz"
            manifest.write_text("manifest")
            archive.write_text("archive")

            with self.assertRaisesRegex(bundle.BundleError, "cannot publish"):
                bundle.publish_bundle_files(
                    (manifest, missing, archive), output_dir
                )

            self.assertFalse((output_dir / manifest.name).exists())
            self.assertFalse((output_dir / archive.name).exists())

    def test_canonical_json_is_stable(self) -> None:
        value = {"z": 1, "a": {"enabled": True}}
        self.assertEqual(
            bundle.canonical_json(value),
            b'{\n  "a": {\n    "enabled": true\n  },\n  "z": 1\n}\n',
        )

    def test_bundle_rejects_unsafe_members(self) -> None:
        cases = {
            "path traversal": self.regular_member("../api", b"api"),
            "setuid mode": self.regular_member("bin/api", b"api", 0o4755),
            "symbolic link": self.symlink_member("bin/api", "/tmp/api"),
        }
        for name, member in cases.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temp:
                path = Path(temp) / "bundle.tar.gz"
                self.write_archive(path, [member])
                with self.assertRaises(bundle.BundleError):
                    bundle.read_bundle(path)

    def test_checksum_parsers_fail_closed(self) -> None:
        valid = "a" * 64
        self.assertEqual(
            bundle.parse_checksums(f"{valid}  bin/api\n".encode()),
            {"bin/api": valid},
        )
        self.assertEqual(
            bundle.parse_sidecar(f"{valid}  bundle.tar.gz\n", "bundle.tar.gz"),
            valid,
        )
        invalid_checksums = [
            f"{valid}  bin/api".encode(),
            f"{valid}  bin/api\r\n".encode(),
            b"not-a-checksum  bin/api\n",
            f"{valid} bin/api\n".encode(),
            f"{valid}  bin/api\n{valid}  bin/api\n".encode(),
        ]
        for value in invalid_checksums:
            with self.subTest(value=value), self.assertRaises(bundle.BundleError):
                bundle.parse_checksums(value)
        invalid_sidecars = [
            f"{valid}  bundle.tar.gz",
            f"{valid}  bundle.tar.gz\r\n",
            f"{valid}  another.tar.gz\n",
            f"{valid}  bundle.tar.gz\n{valid}  bundle.tar.gz\n",
        ]
        for value in invalid_sidecars:
            with self.subTest(value=value), self.assertRaises(bundle.BundleError):
                bundle.parse_sidecar(value, "bundle.tar.gz")

    def test_resolved_manifest_matches_authoritative_contract(self) -> None:
        source = bundle.load_source_manifest(
            CI_DIR / "rest-command-manifest.json", REST_API_DIR
        )
        candidate = "1" * 40
        short_sha = candidate[:8]
        version = "pull-request-4581-11111111"
        timestamp = "2026-08-05T12:34:56Z"
        value = self.resolved_manifest(
            source, "linux-amd64", version, timestamp, candidate, short_sha
        )
        target, outputs = bundle.validate_resolved_manifest(
            value,
            source,
            REST_API_DIR,
            "linux-amd64",
            version,
            timestamp,
            candidate,
            short_sha,
            False,
        )
        self.assertEqual(target, source.targets["linux-amd64"])
        self.assertEqual(len(outputs), 13)

        cases = {
            "wrong target": (
                lambda changed: changed["target"].__setitem__(
                    "name", "linux-arm64"
                ),
                "resolved manifest target is",
            ),
            "wrong candidate": (
                lambda changed: changed.__setitem__("candidate_sha", "2" * 40),
                "resolved manifest candidate_sha is",
            ),
            "missing output": (
                lambda changed: changed["outputs"].pop(),
                "resolved manifest has 12 outputs, expected 13",
            ),
            "relabeled output": (
                lambda changed: changed["outputs"][0].__setitem__(
                    "name", "../../escape"
                ),
                "resolved manifest outputs[0].name is '../../escape', expected 'api'",
            ),
            "wrong ldflags": (
                lambda changed: changed["outputs"][0].__setitem__("ldflags", []),
                "resolved manifest outputs[0].ldflags is [], expected",
            ),
            "wrong source manifest": (
                lambda changed: changed.__setitem__(
                    "source_manifest_sha256", "3" * 64
                ),
                "resolved manifest source_manifest_sha256 is",
            ),
            "boolean schema version": (
                lambda changed: changed.__setitem__("schema_version", True),
                "resolved manifest has an unsupported schema",
            ),
            "floating-point schema version": (
                lambda changed: changed.__setitem__("schema_version", 1.0),
                "resolved manifest has an unsupported schema",
            ),
        }
        for name, (mutate, expected_error) in cases.items():
            with self.subTest(name=name):
                changed = copy.deepcopy(value)
                mutate(changed)
                with self.assertRaisesRegex(
                    bundle.BundleError, re.escape(expected_error)
                ):
                    bundle.validate_resolved_manifest(
                        changed,
                        source,
                        REST_API_DIR,
                        "linux-amd64",
                        version,
                        timestamp,
                        candidate,
                        short_sha,
                        False,
                    )

    @staticmethod
    def resolved_manifest(
        source: bundle.SourceManifest,
        target_name: str,
        version: str,
        timestamp: str,
        candidate: str,
        short_sha: str,
    ) -> dict[str, object]:
        parsed_timestamp = bundle.parse_build_timestamp(timestamp)
        values = {
            "BUILD_TIME_LEGACY": parsed_timestamp.strftime("%Y-%m-%d %H:%M:%S"),
            "BUILD_TIME_RFC3339": timestamp,
            "SHORT_SHA": short_sha,
            "VERSION": version,
        }
        module = bundle.module_path(REST_API_DIR)
        outputs = []
        for output in source.outputs:
            if output.target != target_name:
                continue
            outputs.append(
                {
                    "cgo_enabled": output.cgo_enabled,
                    "file_description": "test binary",
                    "go_version": "go1.26.4",
                    "import_path": bundle.expected_import_path(module, output.package),
                    "ldflags": list(bundle.resolve_ldflags(output, values)),
                    "name": output.name,
                    "output": output.archive_path,
                    "package": output.package,
                    "sha256": "a" * 64,
                    "size": 1,
                    "vcs_modified": "unknown",
                    "vcs_revision": "unavailable",
                }
            )
        target = source.targets[target_name]
        return {
            "build_timestamp": timestamp,
            "candidate_sha": candidate,
            "outputs": outputs,
            "schema_version": 1,
            "short_sha": short_sha,
            "source_dirty": False,
            "source_manifest_sha256": source.sha256,
            "target": {
                "goarch": target.goarch,
                "goos": target.goos,
                "name": target.name,
            },
            "version": version,
        }

    @staticmethod
    def regular_member(
        name: str, value: bytes, mode: int = 0o755
    ) -> tuple[tarfile.TarInfo, bytes]:
        member = tarfile.TarInfo(name)
        member.size = len(value)
        member.mode = mode
        return member, value

    @staticmethod
    def symlink_member(name: str, target: str) -> tuple[tarfile.TarInfo, bytes]:
        member = tarfile.TarInfo(name)
        member.type = tarfile.SYMTYPE
        member.linkname = target
        return member, b""

    @staticmethod
    def write_archive(
        path: Path, members: list[tuple[tarfile.TarInfo, bytes]]
    ) -> None:
        with path.open("wb") as destination:
            with gzip.GzipFile(filename="", mode="wb", fileobj=destination, mtime=0) as zipped:
                with tarfile.open(fileobj=zipped, mode="w") as archive:
                    for member, value in members:
                        archive.addfile(member, io.BytesIO(value))


if __name__ == "__main__":
    unittest.main()
