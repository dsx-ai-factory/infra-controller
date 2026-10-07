# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Check files included in Core boot and ephemeral artifact uploads.

Each active build and architecture has an explicit list of files that must be
present before GitHub Actions uploads the production bundle. That includes
downloaded boot inputs carried into an ephemeral bundle. Build logs remain
optional diagnostics, and intermediate files that are never uploaded are
outside this check.
"""

from __future__ import annotations

import argparse
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class BuildIdentity:
    """Reusable-workflow inputs that select one artifact builder.

    Attributes:
        cargo_make_task: Exact cargo-make task executed by the workflow.
        build_type: Artifact family reported by the workflow.
        arch: Architecture reported by the workflow.
    """

    cargo_make_task: str
    build_type: str
    arch: str


@dataclass(frozen=True)
class ArtifactContract:
    """Files one builder must produce, may omit, or must not produce.

    Attributes:
        required: Repository-relative paths or globs. Each pattern must match
            at least one nonempty regular file, and every matching path is
            checked. Neither the file nor any parent below the repository root
            may be a symlink.
        optional: Repository-relative diagnostic paths that appear in the
            printed contract but do not affect validation. These files, such
            as `build.log`, are not release inputs.
        disabled: Repository-relative paths or globs that must not match
            anything. A match means the CI builder performed work that is
            deliberately outside its scope.
    """

    required: tuple[str, ...]
    optional: tuple[str, ...] = ()
    disabled: tuple[str, ...] = ()


# CI boot jobs intentionally omit the admin CLI because no downstream job uses
# it. The workflow runs in a clean checkout, so its presence means a redundant
# workspace build was reintroduced into this job.
UNBUILT_ADMIN_CLI = "target/debug/nico-admin-cli"


BOOT_REQUIRED_OUTPUTS: dict[str, tuple[str, ...]] = {
    "x86_64": (
        "pxe/static/blobs/internal/x86_64/ipxe.efi",
        "pxe/static/blobs/internal/x86_64/golan.efi",
        "pxe/static/blobs/internal/apt/dists/focal/Release",
        "pxe/static/blobs/internal/apt/dists/focal/main/binary-amd64/Packages",
        "pxe/static/blobs/internal/apt/dists/focal/main/binary-amd64/Release",
        "pxe/static/blobs/internal/apt/pool/base/f/forge-scout/forge-scout_*_amd64.deb",
        "target/debs/forge-scout_*_amd64.deb",
    ),
    "aarch64": (
        "pxe/static/blobs/internal/aarch64/secure-boot-pk.pem",
        "pxe/static/blobs/internal/aarch64/ipxe.efi",
        "pxe/static/blobs/internal/aarch64/carbide.efi",
        "pxe/static/blobs/internal/aarch64/carbide.root",
        "pxe/static/blobs/internal/aarch64/preingestion.bfb",
        "pxe/static/blobs/internal/aarch64/forge.bfb",
        "pxe/static/blobs/internal/apt/dists/focal/Release",
        "pxe/static/blobs/internal/apt/dists/focal/main/binary-arm64/Packages",
        "pxe/static/blobs/internal/apt/dists/focal/main/binary-arm64/Release",
        "pxe/static/blobs/internal/apt/pool/base/f/forge-dpu/forge-dpu_*_arm64.deb",
        "pxe/static/blobs/internal/apt/pool/base/f/forge-scout/forge-scout_*_arm64.deb",
        "target/aarch64-unknown-linux-gnu/release/forge-scout",
        "target/debs/forge-dpu_*_arm64.deb",
        "target/debs/forge-scout_*_arm64.deb",
    ),
}


EPHEMERAL_GENERATED_OUTPUTS: dict[str, tuple[str, ...]] = {
    "x86_64": (
        "pxe/static/blobs/internal/x86_64/scout.efi",
        "pxe/static/blobs/internal/x86_64/scout.squashfs",
        "pxe/static/blobs/internal/x86_64/qcow-imager.efi",
    ),
    "aarch64": (
        "pxe/static/blobs/internal/aarch64/scout.efi",
        "pxe/static/blobs/internal/aarch64/scout.squashfs",
        "pxe/static/blobs/internal/aarch64/qcow-imager.efi",
    ),
}


# These contracts describe the five tasks selected by Core CI:
# `build-boot-artifacts-x86-host-ci`, `build-boot-artifacts-bfb-ci`,
# `package-scout-aarch64-ci`, `create-ephemeral-image-x86-host-ci`, and
# `create-ephemeral-image-arm-host-ci`. Each key also locks the task to its
# expected build type and architecture. A new caller must use one complete
# identity below or add a contract that describes its actual outputs.
CONTRACTS: dict[BuildIdentity, ArtifactContract] = {
    BuildIdentity(
        cargo_make_task="build-boot-artifacts-x86-host-ci",
        build_type="boot",
        arch="x86_64",
    ): ArtifactContract(
        required=BOOT_REQUIRED_OUTPUTS["x86_64"],
        disabled=(UNBUILT_ADMIN_CLI,),
    ),
    BuildIdentity(
        cargo_make_task="build-boot-artifacts-bfb-ci",
        build_type="boot",
        arch="aarch64",
    ): ArtifactContract(
        required=BOOT_REQUIRED_OUTPUTS["aarch64"],
        disabled=(UNBUILT_ADMIN_CLI,),
    ),
    BuildIdentity(
        cargo_make_task="create-ephemeral-image-x86-host-ci",
        build_type="ephemeral",
        arch="x86_64",
    ): ArtifactContract(
        # The ephemeral upload preserves the downloaded boot bundle beside its
        # new images, so validate the carried inputs before uploading them too.
        required=(
            BOOT_REQUIRED_OUTPUTS["x86_64"]
            + EPHEMERAL_GENERATED_OUTPUTS["x86_64"]
        ),
        optional=("build.log",),
        disabled=(UNBUILT_ADMIN_CLI,),
    ),
    BuildIdentity(
        cargo_make_task="create-ephemeral-image-arm-host-ci",
        build_type="ephemeral",
        arch="aarch64",
    ): ArtifactContract(
        # Unlike x86_64 above, this job does not carry the boot bundle: it
        # downloads only the forge-scout deb from the package-scout job, and
        # the release-artifacts carrier takes the boot blobs from the bfb
        # job's own artifact.
        required=(
            ("target/debs/forge-scout_*_arm64.deb",)
            + EPHEMERAL_GENERATED_OUTPUTS["aarch64"]
        ),
        optional=("build.log",),
        disabled=(UNBUILT_ADMIN_CLI,),
    ),
    BuildIdentity(
        cargo_make_task="package-scout-aarch64-ci",
        build_type="package",
        arch="aarch64",
    ): ArtifactContract(
        # The deb is the whole deliverable; the symbolication copy of the
        # unstripped binary ships in the bfb job's boot artifact.
        required=("target/debs/forge-scout_*_arm64.deb",),
        disabled=(UNBUILT_ADMIN_CLI,),
    ),
}


def output_has_symlinked_parent(root: Path, relative_output: Path) -> bool:
    """Return whether a parent below `root` is a symlink."""

    current = root
    for part in relative_output.parts[:-1]:
        current /= part
        if current.is_symlink():
            return True

    return False


def validate_outputs(root: Path, contract: ArtifactContract) -> list[str]:
    """Check one builder's files against `contract`.

    All contract patterns are relative to `root`, which is the checked-out
    repository workspace. A required glob may match several files; every match
    must satisfy the same file checks. Validation collects every error so one
    expensive builder run reports all of the files that need attention.

    Args:
        root: Repository workspace containing the built files.
        contract: Required, optional, and disabled paths for this builder.

    Returns:
        Every required-output or disabled-output contract error.
    """

    errors: list[str] = []
    for pattern in contract.required:
        matches = sorted(root.glob(pattern))
        if not matches:
            errors.append(f"missing required output: {pattern}")
            continue

        for output in matches:
            relative_output = output.relative_to(root)
            # `Path.is_file()` follows a symlink to its target. Reject the
            # output and its parents first so an indirect file cannot satisfy
            # this contract.
            if output_has_symlinked_parent(root, relative_output):
                errors.append(
                    f"required output has a symlinked parent: {relative_output}"
                )
            elif output.is_symlink():
                errors.append(f"required output is a symlink: {relative_output}")
            elif not output.is_file():
                errors.append(
                    f"required output is not a regular file: {relative_output}"
                )
            elif output.stat().st_size == 0:
                errors.append(f"required output is empty: {relative_output}")

    for pattern in contract.disabled:
        for output in sorted(root.glob(pattern)):
            errors.append(f"disabled output is present: {output.relative_to(root)}")

    return errors


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    """Parse the build identity and repository root to validate."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--cargo-make-task",
        choices=sorted({identity.cargo_make_task for identity in CONTRACTS}),
        required=True,
        help="cargo-make task run by the reusable workflow.",
    )
    parser.add_argument(
        "--build-type",
        choices=sorted({identity.build_type for identity in CONTRACTS}),
        required=True,
        help="Output family built by the workflow.",
    )
    parser.add_argument(
        "--arch",
        choices=sorted({identity.arch for identity in CONTRACTS}),
        required=True,
        help="Architecture built by the workflow.",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path.cwd(),
        help=(
            "Repository workspace containing the pxe/ and target/ directories "
            "(default: current directory)."
        ),
    )
    return parser.parse_args(argv)


def print_paths(label: str, paths: tuple[str, ...]) -> None:
    """Print one contract category in a stable, readable format."""

    print(f"{label} ({len(paths)}):")
    if not paths:
        print("  (none)")
        return

    for path in paths:
        print(f"  - {path}")


def main(argv: Sequence[str] | None = None) -> int:
    """Validate one build and print its complete output classification.

    Returns:
        Zero when the contract passes, one after printing every file error, or
        two when the inputs do not identify a supported builder.
    """

    args = parse_args(argv)
    build_identity = BuildIdentity(
        cargo_make_task=args.cargo_make_task,
        build_type=args.build_type,
        arch=args.arch,
    )
    contract = CONTRACTS.get(build_identity)
    if contract is None:
        print(
            "ERROR: no output contract for "
            f"{build_identity.cargo_make_task} "
            f"({build_identity.build_type}/{build_identity.arch})",
            file=sys.stderr,
        )
        return 2

    print(
        f"Output contract: {build_identity.cargo_make_task} "
        f"({build_identity.build_type}/{build_identity.arch})"
    )
    print_paths("Required outputs", contract.required)
    print_paths("Optional outputs", contract.optional)
    print_paths("Deliberately disabled outputs", contract.disabled)

    errors = validate_outputs(args.root, contract)
    if errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
        return 1

    print(
        f"Validated {len(contract.required)} required and "
        f"{len(contract.disabled)} disabled output pattern(s)."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
