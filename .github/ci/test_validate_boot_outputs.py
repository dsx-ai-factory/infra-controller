# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Contract tests for Core boot and ephemeral output validation.

Run from the repository root with
`python3 -m unittest discover -s .github/ci -p 'test_validate_boot_outputs.py'`.
"""

from __future__ import annotations

import io
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from dataclasses import dataclass
from pathlib import Path

from validate_boot_outputs import (
    CONTRACTS,
    ArtifactContract,
    BuildIdentity,
    main,
    validate_outputs,
)


EXPECTED_BUILD_IDENTITIES = {
    ("boot", "x86_64"): BuildIdentity(
        cargo_make_task="build-boot-artifacts-x86-host-ci",
        build_type="boot",
        arch="x86_64",
    ),
    ("boot", "aarch64"): BuildIdentity(
        cargo_make_task="build-boot-artifacts-bfb-ci",
        build_type="boot",
        arch="aarch64",
    ),
    ("ephemeral", "x86_64"): BuildIdentity(
        cargo_make_task="create-ephemeral-image-x86-host-ci",
        build_type="ephemeral",
        arch="x86_64",
    ),
    ("ephemeral", "aarch64"): BuildIdentity(
        cargo_make_task="create-ephemeral-image-arm-host-ci",
        build_type="ephemeral",
        arch="aarch64",
    ),
    ("package", "aarch64"): BuildIdentity(
        cargo_make_task="package-scout-aarch64-ci",
        build_type="package",
        arch="aarch64",
    ),
}


@dataclass(frozen=True)
class ValidationCase:
    """Files and expected error for one table-driven contract fixture.

    Attributes:
        name: Description shown by `unittest` when the subtest fails.
        omit: Required pattern whose fixture file should not be written.
        empty: Required pattern whose fixture file should contain zero bytes.
        include_optional: Whether to write every optional diagnostic file.
        include_disabled: Whether to write every deliberately disabled file.
        expected_error: Error this fixture must report, or `None` when the
            contract should pass without errors.
    """

    name: str
    omit: str | None = None
    empty: str | None = None
    include_optional: bool = False
    include_disabled: bool = False
    expected_error: str | None = None


def concrete_path(pattern: str) -> Path:
    """Replace each `*` in a contract pattern with a stable fixture name."""

    return Path(pattern.replace("*", "fixture"))


def write_output(root: Path, pattern: str, content: bytes = b"artifact") -> None:
    """Write one fixture file for an exact path or `*` contract pattern.

    Args:
        root: Temporary repository root for the fixture.
        pattern: Exact path or glob to turn into one concrete fixture path.
        content: Bytes written to the fixture file.
    """

    output = root / concrete_path(pattern)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(content)


def populate_contract(
    root: Path,
    contract: ArtifactContract,
    *,
    omit: str | None = None,
    empty: str | None = None,
    include_optional: bool = False,
    include_disabled: bool = False,
) -> None:
    """Write the requested contract fixture under `root`.

    Required files are written by default. `omit` and `empty` select one
    required pattern to remove or empty, while the two booleans opt into files
    that are absent from an ordinary successful fixture.

    Args:
        root: Temporary repository root for the fixture.
        contract: Paths used to populate the fixture.
        omit: Required pattern to leave absent.
        empty: Required pattern to create as a zero-byte file.
        include_optional: Whether to write optional diagnostic files.
        include_disabled: Whether to write deliberately disabled files.
    """

    for pattern in contract.required:
        if pattern == omit:
            continue
        write_output(root, pattern, b"" if pattern == empty else b"artifact")

    if include_optional:
        for pattern in contract.optional:
            write_output(root, pattern)

    if include_disabled:
        for pattern in contract.disabled:
            write_output(root, pattern)


class ValidateBootOutputsTests(unittest.TestCase):
    """Verify every builder contract and its filesystem failure modes."""

    def test_contracts_classify_every_active_build_once(self) -> None:
        self.assertEqual(set(CONTRACTS), set(EXPECTED_BUILD_IDENTITIES.values()))

        for build_identity, contract in CONTRACTS.items():
            with self.subTest(build_identity=build_identity):
                required = set(contract.required)
                optional = set(contract.optional)
                disabled = set(contract.disabled)

                self.assertTrue(required)
                self.assertEqual(len(contract.required), len(required))
                self.assertEqual(required & optional, set())
                self.assertEqual(required & disabled, set())
                self.assertEqual(optional & disabled, set())

    def test_required_outputs_are_inside_the_uploaded_paths(self) -> None:
        # This mirrors the `Upload artifacts` path list in
        # build-boot-artifacts.yml. Update both when that list changes.
        upload_paths = (
            "pxe/static/blobs/",
            "target/debs/",
            "target/aarch64-unknown-linux-gnu/release/forge-scout",
        )

        for build_identity, contract in CONTRACTS.items():
            with self.subTest(build_identity=build_identity):
                outside_upload = {
                    pattern
                    for pattern in contract.required
                    if not any(pattern.startswith(path) for path in upload_paths)
                }
                self.assertEqual(outside_upload, set())

    def test_every_active_contract_accepts_complete_outputs(self) -> None:
        for build_identity, contract in CONTRACTS.items():
            with self.subTest(build_identity=build_identity):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    populate_contract(root, contract, include_optional=True)

                    self.assertEqual(validate_outputs(root, contract), [])

    def test_ephemeral_contracts_keep_downloaded_boot_inputs(self) -> None:
        # Keep this inventory independent from the production constants. A
        # handoff change must update this reviewable contract too. The x86_64
        # job downloads the full boot bundle and re-uploads it; the aarch64
        # job downloads only the forge-scout deb from the package-scout job.
        expected_boot_inputs = {
            "x86_64": {
                "pxe/static/blobs/internal/x86_64/ipxe.efi",
                "pxe/static/blobs/internal/x86_64/golan.efi",
                "pxe/static/blobs/internal/apt/dists/focal/Release",
                "pxe/static/blobs/internal/apt/dists/focal/main/binary-amd64/Packages",
                "pxe/static/blobs/internal/apt/dists/focal/main/binary-amd64/Release",
                "pxe/static/blobs/internal/apt/pool/base/f/forge-scout/forge-scout_*_amd64.deb",
                "target/debs/forge-scout_*_amd64.deb",
            },
            "aarch64": {
                "target/debs/forge-scout_*_arm64.deb",
            },
        }
        expected_generated_outputs = {
            "x86_64": {
                "pxe/static/blobs/internal/x86_64/scout.efi",
                "pxe/static/blobs/internal/x86_64/scout.squashfs",
                "pxe/static/blobs/internal/x86_64/qcow-imager.efi",
            },
            "aarch64": {
                "pxe/static/blobs/internal/aarch64/scout.efi",
                "pxe/static/blobs/internal/aarch64/scout.squashfs",
                "pxe/static/blobs/internal/aarch64/qcow-imager.efi",
            },
        }

        for arch, boot_inputs in expected_boot_inputs.items():
            with self.subTest(arch=arch):
                identity = EXPECTED_BUILD_IDENTITIES[("ephemeral", arch)]
                ephemeral_outputs = set(CONTRACTS[identity].required)
                self.assertEqual(
                    ephemeral_outputs,
                    boot_inputs | expected_generated_outputs[arch],
                )

    def test_ephemeral_contracts_reject_missing_boot_inputs(self) -> None:
        boot_inputs = {
            "x86_64": "pxe/static/blobs/internal/x86_64/ipxe.efi",
            "aarch64": "target/debs/forge-scout_*_arm64.deb",
        }

        for arch, boot_input in boot_inputs.items():
            with self.subTest(arch=arch):
                identity = EXPECTED_BUILD_IDENTITIES[("ephemeral", arch)]
                contract = CONTRACTS[identity]
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    populate_contract(root, contract, omit=boot_input)

                    self.assertEqual(
                        validate_outputs(root, contract),
                        [f"missing required output: {boot_input}"],
                    )

    def test_required_optional_and_disabled_cases(self) -> None:
        contract = CONTRACTS[EXPECTED_BUILD_IDENTITIES[("ephemeral", "x86_64")]]
        required_output = contract.required[0]
        cases = (
            ValidationCase("complete outputs", include_optional=True),
            ValidationCase(
                "missing required output",
                omit=required_output,
                expected_error=f"missing required output: {required_output}",
            ),
            ValidationCase(
                "empty required output",
                empty=required_output,
                expected_error=f"required output is empty: {required_output}",
            ),
            ValidationCase("optional output absent"),
            ValidationCase(
                "disabled output present",
                include_optional=True,
                include_disabled=True,
                expected_error=f"disabled output is present: {contract.disabled[0]}",
            ),
        )

        for case in cases:
            with self.subTest(case=case.name):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    populate_contract(
                        root,
                        contract,
                        omit=case.omit,
                        empty=case.empty,
                        include_optional=case.include_optional,
                        include_disabled=case.include_disabled,
                    )

                    errors = validate_outputs(root, contract)

                    if case.expected_error is None:
                        self.assertEqual(errors, [])
                    else:
                        self.assertEqual(errors, [case.expected_error])

    def test_required_output_must_be_a_regular_file(self) -> None:
        contract = CONTRACTS[EXPECTED_BUILD_IDENTITIES[("ephemeral", "x86_64")]]
        required_output = contract.required[0]

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            populate_contract(root, contract)
            output = root / concrete_path(required_output)
            output.unlink()
            output.mkdir()

            self.assertEqual(
                validate_outputs(root, contract),
                [f"required output is not a regular file: {required_output}"],
            )

    def test_required_output_must_not_be_a_symlink(self) -> None:
        contract = CONTRACTS[EXPECTED_BUILD_IDENTITIES[("ephemeral", "x86_64")]]
        required_output = contract.required[0]

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            populate_contract(root, contract)
            output = root / concrete_path(required_output)
            symlink_target = root / "symlink-target"
            symlink_target.write_bytes(b"artifact")
            output.unlink()
            output.symlink_to(symlink_target)

            self.assertEqual(
                validate_outputs(root, contract),
                [f"required output is a symlink: {required_output}"],
            )

    def test_required_output_must_not_have_a_symlinked_parent(self) -> None:
        required_output = "pxe/static/blobs/internal/x86_64/ipxe.efi"
        contract = ArtifactContract(required=(required_output,))

        with tempfile.TemporaryDirectory() as directory:
            fixture_root = Path(directory)
            workspace = fixture_root / "workspace"
            external = fixture_root / "external"
            workspace.mkdir()
            write_output(external, required_output)
            (workspace / "pxe").symlink_to(external / "pxe", target_is_directory=True)

            self.assertEqual(
                validate_outputs(workspace, contract),
                [f"required output has a symlinked parent: {required_output}"],
            )

    def test_every_match_for_a_required_glob_must_be_nonempty(self) -> None:
        pattern = "target/debs/forge-scout_*.deb"
        contract = ArtifactContract(required=(pattern,))

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_output(root, "target/debs/forge-scout_valid.deb")
            write_output(root, "target/debs/forge-scout_empty.deb", b"")

            self.assertEqual(
                validate_outputs(root, contract),
                ["required output is empty: target/debs/forge-scout_empty.deb"],
            )

    def test_main_returns_success_and_failure_statuses(self) -> None:
        identity = EXPECTED_BUILD_IDENTITIES[("ephemeral", "x86_64")]
        contract = CONTRACTS[identity]
        cases = (
            ("complete outputs", True, 0),
            ("missing outputs", False, 1),
        )

        for case_name, should_populate, expected_status in cases:
            with self.subTest(case=case_name):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    if should_populate:
                        populate_contract(root, contract)

                    stdout = io.StringIO()
                    stderr = io.StringIO()
                    with redirect_stdout(stdout), redirect_stderr(stderr):
                        status = main(
                            (
                                "--cargo-make-task",
                                identity.cargo_make_task,
                                "--build-type",
                                identity.build_type,
                                "--arch",
                                identity.arch,
                                "--root",
                                str(root),
                            )
                        )

                    self.assertEqual(status, expected_status)
                    self.assertIn(
                        "Output contract: "
                        f"{identity.cargo_make_task} "
                        f"({identity.build_type}/{identity.arch})",
                        stdout.getvalue(),
                    )
                    if expected_status == 0:
                        self.assertIn(
                            f"Validated {len(contract.required)} required and "
                            f"{len(contract.disabled)} disabled output pattern(s).",
                            stdout.getvalue(),
                        )
                        self.assertEqual(stderr.getvalue(), "")
                    else:
                        self.assertNotIn("Validated ", stdout.getvalue())
                        self.assertIn(
                            f"ERROR: missing required output: {contract.required[0]}",
                            stderr.getvalue(),
                        )

    def test_main_rejects_a_mismatched_build_identity(self) -> None:
        identity = EXPECTED_BUILD_IDENTITIES[("boot", "x86_64")]
        stderr = io.StringIO()

        with redirect_stderr(stderr):
            status = main(
                (
                    "--cargo-make-task",
                    identity.cargo_make_task,
                    "--build-type",
                    identity.build_type,
                    "--arch",
                    "aarch64",
                )
            )

        self.assertEqual(status, 2)
        self.assertIn(
            "ERROR: no output contract for "
            f"{identity.cargo_make_task} (boot/aarch64)",
            stderr.getvalue(),
        )
