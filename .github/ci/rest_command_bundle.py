#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Build and verify the REST command artifacts for one target.

The checked-in manifest owns the build contract. ``build`` turns that contract
into one archive, checksum, and resolved manifest; ``verify`` reopens those
files and checks them without trusting the archive to describe itself.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import re
import shlex
import subprocess
import sys
import tarfile
import tempfile
from collections.abc import Iterable
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from string import Template
from typing import Any


SCHEMA_VERSION = 1
TOKEN_RE = re.compile(r"\$\{([A-Z][A-Z0-9_]*)\}")
ALLOWED_TOKENS = {
    "BUILD_TIME_LEGACY",
    "BUILD_TIME_RFC3339",
    "SHORT_SHA",
    "VERSION",
}
NAME_RE = re.compile(r"[a-z0-9](?:[a-z0-9-]*[a-z0-9])?")
FULL_SHA_RE = re.compile(r"[0-9a-f]{40}")
SHORT_SHA_RE = re.compile(r"[0-9a-f]{7,12}")
HASH_RE = re.compile(r"[0-9a-f]{64}")
VERSION_RE = re.compile(r"[a-zA-Z0-9][a-zA-Z0-9._-]*")
BUILD_TIMESTAMP_RE = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z")
DEFAULT_MANIFEST = Path(__file__).resolve().with_name("rest-command-manifest.json")
DEFAULT_REPO_ROOT = Path(__file__).resolve().parents[2] / "rest-api"
PHYSICAL_ARCHITECTURE_MARKERS = {
    "linux-amd64": ("ELF 64-bit", "x86-64"),
    "linux-arm64": ("ELF 64-bit", "ARM aarch64"),
    "darwin-arm64": ("Mach-O 64-bit", "arm64"),
}
STATIC_LINUX_LINKAGE_MARKERS = ("statically linked", "static-pie linked")


class BundleError(RuntimeError):
    """``BundleError`` turns a bad contract, build, or bundle into one CLI error."""


@dataclass(frozen=True)
class Target:
    """``Target`` maps a manifest name to one Go OS and architecture pair."""

    name: str
    goos: str
    goarch: str


@dataclass(frozen=True)
class CommandBuild:
    """``CommandBuild`` is one command's complete contract for one target."""

    name: str
    package: str
    target: str
    cgo_enabled: bool
    ldflags: tuple[str, ...]
    archive_path: str


@dataclass(frozen=True)
class SourceManifest:
    """``SourceManifest`` is the validated contract loaded from checked-in JSON."""

    targets: dict[str, Target]
    outputs: tuple[CommandBuild, ...]
    sha256: str


@dataclass(frozen=True)
class BinaryInspection:
    """``BinaryInspection`` records facts read back from one compiled command."""

    go_version: str
    vcs_modified: str
    vcs_revision: str
    file_description: str


def sha256_bytes(value: bytes) -> str:
    """Return the lowercase SHA-256 digest for an in-memory value."""

    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    """Hash `path` in bounded chunks so file size does not set memory use."""

    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def require_keys(value: dict[str, Any], expected: set[str], context: str) -> None:
    """Reject missing or unknown object fields at a named contract boundary."""

    actual = set(value)
    if actual != expected:
        raise BundleError(
            f"{context} keys must be {sorted(expected)}, got {sorted(actual)}"
        )


def require_string(value: Any, field: str) -> str:
    """Return one safe, nonempty single-line string or reject `field`."""

    if not isinstance(value, str) or not value:
        raise BundleError(f"{field} must be a non-empty string")
    if "\n" in value or "\r" in value or "\0" in value:
        raise BundleError(f"{field} must be a single-line string without NUL bytes")
    return value


def parse_target(value: Any, index: int) -> Target:
    """Validate one source-manifest target and return its typed form."""

    if not isinstance(value, dict):
        raise BundleError(f"targets[{index}] must be an object")
    require_keys(value, {"name", "goos", "goarch"}, f"targets[{index}]")
    name = require_string(value["name"], f"targets[{index}].name")
    goos = require_string(value["goos"], f"targets[{index}].goos")
    goarch = require_string(value["goarch"], f"targets[{index}].goarch")
    if name != f"{goos}-{goarch}":
        raise BundleError(
            f"targets[{index}].name must be {goos}-{goarch}, got {name}"
        )
    if name not in PHYSICAL_ARCHITECTURE_MARKERS:
        raise BundleError(f"targets[{index}] is not supported: {name}")
    return Target(name=name, goos=goos, goarch=goarch)


def parse_output(value: Any, index: int, repo_root: Path) -> CommandBuild:
    """Validate one command declaration without allowing paths outside REST."""

    if not isinstance(value, dict):
        raise BundleError(f"outputs[{index}] must be an object")
    require_keys(
        value,
        {"name", "package", "target", "cgo_enabled", "ldflags", "output"},
        f"outputs[{index}]",
    )
    name = require_string(value["name"], f"outputs[{index}].name")
    if NAME_RE.fullmatch(name) is None:
        raise BundleError(f"outputs[{index}].name is not a safe command name: {name}")

    package = require_string(value["package"], f"outputs[{index}].package")
    package_parts = package.removeprefix("./").split("/")
    if not package.startswith("./") or any(
        part in {"", ".", ".."} for part in package_parts
    ):
        raise BundleError(
            f"outputs[{index}].package must be a normalized relative ./ path: {package}"
        )
    package_dir = repo_root.joinpath(*package_parts).resolve()
    try:
        package_dir.relative_to(repo_root)
    except ValueError as error:
        raise BundleError(
            f"outputs[{index}].package escapes the repository: {package}"
        ) from error
    if not package_dir.is_dir():
        raise BundleError(f"outputs[{index}].package does not exist: {package}")

    target = require_string(value["target"], f"outputs[{index}].target")
    if not isinstance(value["cgo_enabled"], bool):
        raise BundleError(f"outputs[{index}].cgo_enabled must be a boolean")

    raw_ldflags = value["ldflags"]
    if not isinstance(raw_ldflags, list) or not all(
        isinstance(flag, str) for flag in raw_ldflags
    ):
        raise BundleError(f"outputs[{index}].ldflags must be a list of strings")
    for flag in raw_ldflags:
        require_string(flag, f"outputs[{index}].ldflags")
        if flag == "-X" or flag.startswith("-X "):
            raise BundleError(
                f"outputs[{index}].ldflags must use the -X=name=value form: {flag}"
            )
        tokens = set(TOKEN_RE.findall(flag))
        unknown = tokens - ALLOWED_TOKENS
        if unknown:
            raise BundleError(
                f"outputs[{index}].ldflags uses unknown tokens: {sorted(unknown)}"
            )
        if "$" in TOKEN_RE.sub("", flag):
            raise BundleError(
                f"outputs[{index}].ldflags contains an invalid template token: {flag}"
            )

    output = require_string(value["output"], f"outputs[{index}].output")
    if output != f"bin/{name}":
        raise BundleError(
            f"outputs[{index}].output must be bin/{name}, got {output}"
        )

    return CommandBuild(
        name=name,
        package=package,
        target=target,
        cgo_enabled=value["cgo_enabled"],
        ldflags=tuple(raw_ldflags),
        archive_path=output,
    )


def load_source_manifest(path: Path, repo_root: Path) -> SourceManifest:
    """Load the source manifest and reject contracts the builder cannot enforce."""

    repo_root = repo_root.resolve()
    manifest_bytes = path.read_bytes()
    try:
        manifest_data = json.loads(manifest_bytes)
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        raise BundleError(f"invalid JSON in {path}: {error}") from error
    if not isinstance(manifest_data, dict):
        raise BundleError("manifest root must be an object")
    require_keys(
        manifest_data, {"schema_version", "targets", "outputs"}, "manifest"
    )
    schema_version = manifest_data["schema_version"]
    if (
        not isinstance(schema_version, int)
        or isinstance(schema_version, bool)
        or schema_version != SCHEMA_VERSION
    ):
        raise BundleError(
            f"manifest schema_version must be integer {SCHEMA_VERSION}, "
            f"got {schema_version!r}"
        )
    if not isinstance(manifest_data["targets"], list) or not manifest_data["targets"]:
        raise BundleError("manifest targets must be a non-empty list")
    if not isinstance(manifest_data["outputs"], list) or not manifest_data["outputs"]:
        raise BundleError("manifest outputs must be a non-empty list")

    targets: dict[str, Target] = {}
    for index, raw_target in enumerate(manifest_data["targets"]):
        target = parse_target(raw_target, index)
        if target.name in targets:
            raise BundleError(f"duplicate target: {target.name}")
        targets[target.name] = target

    outputs: list[CommandBuild] = []
    identities: set[tuple[str, str]] = set()
    paths: set[tuple[str, str]] = set()
    used_targets: set[str] = set()
    for index, raw_output in enumerate(manifest_data["outputs"]):
        output = parse_output(raw_output, index, repo_root)
        if output.target not in targets:
            raise BundleError(
                f"outputs[{index}].target is not declared: {output.target}"
            )
        identity = (output.target, output.name)
        if identity in identities:
            raise BundleError(f"duplicate target/name pair: {identity}")
        identities.add(identity)
        output_path = (output.target, output.archive_path)
        if output_path in paths:
            raise BundleError(f"duplicate target/output pair: {output_path}")
        paths.add(output_path)
        used_targets.add(output.target)
        outputs.append(output)
    unused_targets = set(targets) - used_targets
    if unused_targets:
        raise BundleError(f"targets have no outputs: {sorted(unused_targets)}")

    return SourceManifest(
        targets=targets,
        outputs=tuple(outputs),
        sha256=sha256_bytes(manifest_bytes),
    )


def parse_build_timestamp(value: str) -> datetime:
    """Parse the canonical UTC timestamp and require gzip's supported range."""

    require_string(value, "build timestamp")
    if BUILD_TIMESTAMP_RE.fullmatch(value) is None:
        raise BundleError(
            f"build timestamp must use UTC RFC3339 seconds, got {value}"
        )
    try:
        parsed = datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError as error:
        raise BundleError(
            f"build timestamp must use UTC RFC3339 seconds, got {value}"
        ) from error
    timestamp = parsed.replace(tzinfo=timezone.utc)
    gzip_seconds = int(timestamp.timestamp())
    if not 0 <= gzip_seconds <= 0xFFFFFFFF:
        raise BundleError(
            "build timestamp must fit the gzip timestamp range "
            "1970-01-01T00:00:00Z through 2106-02-07T06:28:15Z"
        )
    return timestamp


def validate_identity(version: str, candidate_sha: str, short_sha: str) -> None:
    """Require a safe version and one internally consistent commit identity."""

    require_string(version, "version")
    if VERSION_RE.fullmatch(version) is None:
        raise BundleError(
            "version must start with an alphanumeric character and contain only "
            "letters, numbers, periods, underscores, or hyphens"
        )
    if FULL_SHA_RE.fullmatch(candidate_sha) is None:
        raise BundleError("candidate SHA must be 40 lowercase hexadecimal characters")
    if SHORT_SHA_RE.fullmatch(short_sha) is None:
        raise BundleError("short SHA must be 7 to 12 lowercase hexadecimal characters")
    if not candidate_sha.startswith(short_sha):
        raise BundleError("short SHA does not match the candidate SHA")


def resolve_ldflags(output: CommandBuild, values: dict[str, str]) -> tuple[str, ...]:
    """Substitute the allowed build values into one command's linker flags."""

    resolved: list[str] = []
    for flag in output.ldflags:
        try:
            value = Template(flag).substitute(values)
        except (KeyError, ValueError) as error:
            raise BundleError(
                f"cannot resolve ldflags for {output.target}/{output.name}: {error}"
            ) from error
        require_string(value, f"resolved ldflag for {output.target}/{output.name}")
        resolved.append(value)
    return tuple(resolved)


def command_output(command: list[str], cwd: Path) -> str:
    """Run a read-only inspection command and convert failures to BundleError."""

    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or error.stdout or "command failed").strip()
        raise BundleError(f"{' '.join(command)}: {detail}") from error
    except OSError as error:
        raise BundleError(f"cannot run {command[0]}: {error}") from error
    return result.stdout.strip()


def module_path(repo_root: Path) -> str:
    """Read the Go module path that turns package paths into import paths."""

    for line in (repo_root / "go.mod").read_text(encoding="utf-8").splitlines():
        if line.startswith("module "):
            return line.removeprefix("module ").strip()
    raise BundleError(f"module declaration not found in {repo_root / 'go.mod'}")


def expected_import_path(module: str, package: str) -> str:
    """Return the full Go import path for a normalized local package."""

    suffix = package.removeprefix("./")
    return f"{module}/{suffix}"


def build_info(path: Path, repo_root: Path) -> dict[str, Any]:
    """Read structured Go build metadata back from one compiled command."""

    raw = command_output(["go", "version", "-m", "-json", str(path)], repo_root)
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise BundleError(f"invalid build info for {path}: {error}") from error
    if not isinstance(value, dict):
        raise BundleError(f"build info for {path} is not an object")
    return value


def metadata_values(ldflags: Iterable[str]) -> tuple[str, ...]:
    """Extract values from the supported `-X=name=value` linker-flag form."""

    values: list[str] = []
    for flag in ldflags:
        if not flag.startswith("-X="):
            if flag == "-X" or flag.startswith("-X "):
                raise BundleError(
                    f"-X linker flags must use the -X=name=value form: {flag}"
                )
            continue
        assignment = flag.removeprefix("-X=")
        _, separator, value = assignment.partition("=")
        if not separator or not value:
            raise BundleError(f"invalid -X linker flag: {flag}")
        values.append(value)
    return tuple(values)


def validate_file_description(
    description: str,
    target: Target,
    output: CommandBuild,
) -> None:
    """Check the architecture and linkage facts reported by ``file``.

    ``file`` has used both ``statically linked`` and ``static-pie linked`` for
    valid static ELF binaries. The contract cares about that fact, not which
    libmagic wording the runner happens to use.
    """

    architecture_markers = PHYSICAL_ARCHITECTURE_MARKERS.get(target.name)
    if architecture_markers is None:
        raise BundleError(f"no physical architecture check for {target.name}")
    missing = [marker for marker in architecture_markers if marker not in description]
    if missing:
        raise BundleError(
            f"{output.name} has wrong file type {description!r}; missing {missing}"
        )

    # CGO_ENABLED=1 does not promise dynamic linkage. The build-info check owns
    # that setting; only CGO-disabled Linux commands promise a static file.
    if target.goos != "linux" or output.cgo_enabled:
        return

    if not any(marker in description for marker in STATIC_LINUX_LINKAGE_MARKERS):
        raise BundleError(
            f"{output.name} has wrong file type {description!r}; expected one of "
            f"{list(STATIC_LINUX_LINKAGE_MARKERS)}"
        )


def inspect_binary(
    path: Path,
    repo_root: Path,
    target: Target,
    output: CommandBuild,
    ldflags: tuple[str, ...],
    candidate_sha: str,
    import_path: str,
) -> BinaryInspection:
    """Read a binary back and confirm the compiler honored its build contract."""

    go_build_info = build_info(path, repo_root)
    if go_build_info.get("Path") != import_path:
        raise BundleError(
            f"{output.name} package is {go_build_info.get('Path')}, "
            f"expected {import_path}"
        )
    settings = {
        setting.get("Key"): setting.get("Value")
        for setting in go_build_info.get("Settings", [])
        if isinstance(setting, dict)
    }
    expected_settings = {
        "CGO_ENABLED": "1" if output.cgo_enabled else "0",
        "GOARCH": target.goarch,
        "GOOS": target.goos,
    }
    for key, expected in expected_settings.items():
        if settings.get(key) != expected:
            raise BundleError(
                f"{output.name} {key} is {settings.get(key)!r}, expected {expected!r}"
            )
    vcs_revision = settings.get("vcs.revision")
    # Go normally records the repository revision for this module. Keep the
    # fallback for environments where those settings are unavailable;
    # ``build_bundle`` still checks the exact Git HEAD and source state first.
    if vcs_revision is not None and vcs_revision != candidate_sha:
        raise BundleError(
            f"{output.name} vcs.revision is {vcs_revision!r}, expected {candidate_sha!r}"
        )
    recorded_ldflags = tuple(shlex.split(settings.get("-ldflags", "")))
    if recorded_ldflags != ldflags:
        raise BundleError(
            f"{output.name} ldflags are {recorded_ldflags!r}, expected {ldflags!r}"
        )

    file_description = command_output(["file", "-b", str(path)], repo_root)
    validate_file_description(file_description, target, output)

    embedded_values = metadata_values(ldflags)
    if embedded_values:
        binary = path.read_bytes()
        missing_values = [
            value for value in embedded_values if value.encode() not in binary
        ]
        if missing_values:
            raise BundleError(
                f"{output.name} is missing linker metadata values: {missing_values}"
            )

    go_version = go_build_info.get("GoVersion")
    if not isinstance(go_version, str) or not go_version:
        raise BundleError(f"{output.name} build info has no GoVersion")

    return BinaryInspection(
        go_version=go_version,
        vcs_modified=settings.get("vcs.modified", "unknown"),
        vcs_revision=vcs_revision or "unavailable",
        file_description=file_description,
    )


def canonical_json(value: Any) -> bytes:
    """Encode stable, human-readable JSON with one trailing newline."""

    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def write_tar_member(
    archive: tarfile.TarFile,
    name: str,
    value: bytes,
    mode: int,
    mtime: int,
) -> None:
    """Write one byte-backed member with deterministic ownership and time."""

    header = tarfile.TarInfo(name=name)
    header.size = len(value)
    header.mode = mode
    header.mtime = mtime
    header.uid = 0
    header.gid = 0
    header.uname = ""
    header.gname = ""
    archive.addfile(header, fileobj=io.BytesIO(value))


def create_bundle(
    path: Path,
    manifest: bytes,
    checksums: bytes,
    binaries: dict[str, bytes],
    mtime: int,
) -> None:
    """Write a deterministic archive from the resolved contract and binaries."""

    try:
        with path.open("wb") as destination:
            with gzip.GzipFile(
                filename="",
                mode="wb",
                compresslevel=9,
                fileobj=destination,
                mtime=mtime,
            ) as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as archive:
                    write_tar_member(archive, "manifest.json", manifest, 0o644, mtime)
                    write_tar_member(archive, "SHA256SUMS", checksums, 0o644, mtime)
                    for name in sorted(binaries):
                        write_tar_member(archive, name, binaries[name], 0o755, mtime)
    except (tarfile.TarError, OSError) as error:
        raise BundleError(f"cannot write bundle {path}: {error}") from error


def publish_bundle_files(
    staged_paths: Iterable[Path],
    output_dir: Path,
) -> dict[str, Path]:
    """Publish verified files from same-filesystem staging or leave none behind."""

    published: list[Path] = []
    final_paths: dict[str, Path] = {}
    try:
        for staged_path in staged_paths:
            final_path = output_dir / staged_path.name
            staged_path.replace(final_path)
            published.append(final_path)
            final_paths[staged_path.name] = final_path
    except OSError as error:
        cleanup_errors: list[str] = []
        for final_path in reversed(published):
            try:
                final_path.unlink()
            except FileNotFoundError:
                pass
            except OSError as cleanup_error:
                cleanup_errors.append(f"{final_path}: {cleanup_error}")
        if cleanup_errors:
            raise BundleError(
                "cannot publish bundle files and cannot clean partial output: "
                + "; ".join(cleanup_errors)
            ) from error
        raise BundleError(f"cannot publish bundle files: {error}") from error
    return final_paths


def validate_gzip_header(path: Path, expected_mtime: int) -> None:
    """Require the deterministic gzip header written by ``create_bundle``."""

    with path.open("rb") as source:
        header = source.read(10)
    if len(header) != 10 or header[:3] != b"\x1f\x8b\x08":
        raise BundleError("bundle is not a gzip stream")
    if header[3] != 0:
        raise BundleError("bundle gzip header has unexpected optional fields")
    if int.from_bytes(header[4:8], "little") != expected_mtime:
        raise BundleError("bundle gzip timestamp does not match the build timestamp")
    if header[8] != 2:
        raise BundleError("bundle gzip header does not record level-9 compression")


def read_bundle(
    path: Path,
    expected_mtime: int | None = None,
) -> dict[str, tuple[bytes, int]]:
    """Read only deterministic, safe, regular members and retain their modes."""

    entries: dict[str, tuple[bytes, int]] = {}
    member_names: list[str] = []
    try:
        with tarfile.open(path, mode="r:gz") as archive:
            for member in archive.getmembers():
                member_path = PurePosixPath(member.name)
                if (
                    member.name in entries
                    or member_path.is_absolute()
                    or ".." in member_path.parts
                    or not member.isfile()
                    or member.mode & ~0o777
                ):
                    raise BundleError(f"unsafe or duplicate bundle member: {member.name}")
                if (
                    member.uid != 0
                    or member.gid != 0
                    or member.uname != ""
                    or member.gname != ""
                ):
                    raise BundleError(
                        f"bundle member has non-normalized ownership: {member.name}"
                    )
                if expected_mtime is not None and member.mtime != expected_mtime:
                    raise BundleError(
                        f"bundle member has the wrong timestamp: {member.name}"
                    )
                source = archive.extractfile(member)
                if source is None:
                    raise BundleError(f"cannot read bundle member: {member.name}")
                member_names.append(member.name)
                entries[member.name] = (source.read(), member.mode)
    except (tarfile.TarError, OSError) as error:
        raise BundleError(f"cannot read bundle {path}: {error}") from error
    metadata_names = {"manifest.json", "SHA256SUMS"}
    if metadata_names.issubset(member_names):
        expected_order = ["manifest.json", "SHA256SUMS"] + sorted(
            name for name in member_names if name not in metadata_names
        )
        if member_names != expected_order:
            raise BundleError("bundle members are not in deterministic order")
    return entries


def parse_checksums(value: bytes) -> dict[str, str]:
    """Parse the strict GNU-style checksum inventory embedded in a bundle."""

    checksums: dict[str, str] = {}
    if not value.endswith(b"\n") or b"\r" in value:
        raise BundleError("SHA256SUMS must use LF-terminated lines")
    try:
        lines = value.decode("utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise BundleError("SHA256SUMS is not UTF-8") from error
    for line in lines:
        digest, separator, name = line.partition("  ")
        if not separator or HASH_RE.fullmatch(digest) is None or name in checksums:
            raise BundleError(f"invalid SHA256SUMS line: {line!r}")
        checksums[name] = digest
    return checksums


def parse_sidecar(value: str, bundle_name: str) -> str:
    """Return the digest from the archive's exact one-line checksum sidecar."""

    if not value.endswith("\n") or "\r" in value:
        raise BundleError("bundle checksum sidecar must use one LF-terminated line")
    lines = value.splitlines()
    if len(lines) != 1:
        raise BundleError("bundle checksum sidecar must contain exactly one line")
    digest, separator, name = lines[0].partition("  ")
    if not separator or HASH_RE.fullmatch(digest) is None or name != bundle_name:
        raise BundleError("bundle checksum sidecar is invalid")
    return digest


def validate_resolved_manifest(
    resolved_manifest: Any,
    source_manifest: SourceManifest,
    repo_root: Path,
    expected_target: str,
    expected_version: str,
    expected_build_timestamp: str,
    expected_candidate: str,
    expected_short_sha: str,
    expected_source_dirty: bool | None,
) -> tuple[Target, list[dict[str, Any]]]:
    """Check a resolved manifest against the source contract and expected build."""

    if not isinstance(resolved_manifest, dict):
        raise BundleError("resolved manifest root must be an object")
    require_keys(
        resolved_manifest,
        {
            "build_timestamp",
            "candidate_sha",
            "outputs",
            "schema_version",
            "short_sha",
            "source_dirty",
            "source_manifest_sha256",
            "target",
            "version",
        },
        "resolved manifest",
    )
    schema_version = resolved_manifest["schema_version"]
    if (
        not isinstance(schema_version, int)
        or isinstance(schema_version, bool)
        or schema_version != SCHEMA_VERSION
    ):
        raise BundleError("resolved manifest has an unsupported schema")
    validate_identity(expected_version, expected_candidate, expected_short_sha)
    timestamp = parse_build_timestamp(expected_build_timestamp)
    # An uploaded manifest cannot define its own identity. Require every
    # run-specific value to match the candidate the caller asked us to verify.
    exact_identity = {
        "build_timestamp": expected_build_timestamp,
        "candidate_sha": expected_candidate,
        "short_sha": expected_short_sha,
        "source_manifest_sha256": source_manifest.sha256,
        "version": expected_version,
    }
    for field, expected in exact_identity.items():
        if resolved_manifest[field] != expected:
            raise BundleError(
                f"resolved manifest {field} is {resolved_manifest[field]!r}, "
                f"expected {expected!r}"
            )
    if not isinstance(resolved_manifest["source_dirty"], bool):
        raise BundleError("resolved manifest source_dirty must be a boolean")
    if (
        expected_source_dirty is not None
        and resolved_manifest["source_dirty"] is not expected_source_dirty
    ):
        raise BundleError(
            "resolved manifest source_dirty does not match the expected source state"
        )

    # Resolve the target from the checked-in manifest, then require the uploaded
    # target object to say exactly the same thing.
    if expected_target not in source_manifest.targets:
        raise BundleError(
            f"requested target is not in the source manifest: {expected_target}"
        )
    target = source_manifest.targets[expected_target]
    expected_target_value = {
        "goarch": target.goarch,
        "goos": target.goos,
        "name": target.name,
    }
    if resolved_manifest["target"] != expected_target_value:
        raise BundleError(
            f"resolved manifest target is {resolved_manifest['target']!r}, "
            f"expected {expected_target_value!r}"
        )

    resolved_outputs = resolved_manifest["outputs"]
    if not isinstance(resolved_outputs, list) or not resolved_outputs:
        raise BundleError("resolved manifest outputs must be a non-empty list")
    expected_commands = [
        output for output in source_manifest.outputs if output.target == expected_target
    ]
    if len(resolved_outputs) != len(expected_commands):
        raise BundleError(
            f"resolved manifest has {len(resolved_outputs)} outputs, "
            f"expected {len(expected_commands)}"
        )

    template_values = {
        "BUILD_TIME_LEGACY": timestamp.strftime("%Y-%m-%d %H:%M:%S"),
        "BUILD_TIME_RFC3339": timestamp.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "SHORT_SHA": expected_short_sha,
        "VERSION": expected_version,
    }
    go_module_path = module_path(repo_root)
    output_keys = {
        "cgo_enabled",
        "file_description",
        "go_version",
        "import_path",
        "ldflags",
        "name",
        "output",
        "package",
        "sha256",
        "size",
        "vcs_modified",
        "vcs_revision",
    }
    # Each output is checked in source-manifest order. That makes a missing,
    # reordered, or relabeled binary fail before any archive path is used.
    validated_outputs: list[dict[str, Any]] = []
    for index, (raw_output, expected_command) in enumerate(
        zip(resolved_outputs, expected_commands, strict=True)
    ):
        context = f"resolved manifest outputs[{index}]"
        if not isinstance(raw_output, dict):
            raise BundleError(f"{context} must be an object")
        require_keys(raw_output, output_keys, context)
        exact_contract = {
            "cgo_enabled": expected_command.cgo_enabled,
            "import_path": expected_import_path(
                go_module_path, expected_command.package
            ),
            "ldflags": list(resolve_ldflags(expected_command, template_values)),
            "name": expected_command.name,
            "output": expected_command.archive_path,
            "package": expected_command.package,
        }
        for field, expected in exact_contract.items():
            if raw_output[field] != expected:
                raise BundleError(
                    f"{context}.{field} is {raw_output[field]!r}, expected {expected!r}"
                )
        for field in (
            "file_description",
            "go_version",
            "import_path",
            "name",
            "output",
            "package",
            "sha256",
            "vcs_modified",
            "vcs_revision",
        ):
            require_string(raw_output[field], f"{context}.{field}")
        if NAME_RE.fullmatch(raw_output["name"]) is None:
            raise BundleError(f"{context}.name is unsafe: {raw_output['name']!r}")
        if HASH_RE.fullmatch(raw_output["sha256"]) is None:
            raise BundleError(f"{context}.sha256 must be a lowercase SHA-256 digest")
        if (
            not isinstance(raw_output["size"], int)
            or isinstance(raw_output["size"], bool)
            or raw_output["size"] <= 0
        ):
            raise BundleError(f"{context}.size must be a positive integer")
        if not isinstance(raw_output["ldflags"], list) or not all(
            isinstance(flag, str) and flag for flag in raw_output["ldflags"]
        ):
            raise BundleError(f"{context}.ldflags must be a list of strings")
        if not isinstance(raw_output["cgo_enabled"], bool):
            raise BundleError(f"{context}.cgo_enabled must be a boolean")
        if raw_output["vcs_revision"] not in {expected_candidate, "unavailable"}:
            raise BundleError(
                f"{context}.vcs_revision does not match the candidate"
            )
        validated_outputs.append(raw_output)
    return target, validated_outputs


def verify_bundle(
    bundle_path: Path,
    checksum_path: Path,
    manifest_path: Path,
    source_manifest: SourceManifest,
    repo_root: Path,
    expected_target: str,
    expected_version: str,
    expected_build_timestamp: str,
    expected_candidate: str,
    expected_short_sha: str,
    expected_source_dirty: bool | None,
) -> None:
    """Verify one uploaded bundle without trusting its embedded declarations."""

    try:
        checksum_text = checksum_path.read_bytes().decode()
    except UnicodeDecodeError as error:
        raise BundleError("bundle checksum sidecar is not UTF-8") from error
    expected_bundle_hash = parse_sidecar(checksum_text, bundle_path.name)
    actual_bundle_hash = sha256_file(bundle_path)
    if actual_bundle_hash != expected_bundle_hash:
        raise BundleError(
            f"bundle checksum is {actual_bundle_hash}, expected {expected_bundle_hash}"
        )

    resolved_manifest_bytes = manifest_path.read_bytes()
    try:
        resolved_manifest = json.loads(resolved_manifest_bytes)
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        raise BundleError(f"resolved manifest is invalid JSON: {error}") from error
    if canonical_json(resolved_manifest) != resolved_manifest_bytes:
        raise BundleError("resolved manifest does not use canonical JSON encoding")
    target, resolved_outputs = validate_resolved_manifest(
        resolved_manifest,
        source_manifest,
        repo_root,
        expected_target,
        expected_version,
        expected_build_timestamp,
        expected_candidate,
        expected_short_sha,
        expected_source_dirty,
    )

    expected_mtime = int(parse_build_timestamp(expected_build_timestamp).timestamp())
    validate_gzip_header(bundle_path, expected_mtime)
    entries = read_bundle(bundle_path, expected_mtime)
    expected_names = {"manifest.json", "SHA256SUMS"}
    expected_names.update(output["output"] for output in resolved_outputs)
    if set(entries) != expected_names:
        raise BundleError(
            f"bundle members are {sorted(entries)}, expected {sorted(expected_names)}"
        )
    # The sidecar and embedded manifests must be byte-for-byte identical. There
    # is one claimed build contract, rather than an external and internal copy
    # that can quietly disagree.
    if entries["manifest.json"] != (resolved_manifest_bytes, 0o644):
        raise BundleError("embedded manifest differs from the external manifest")
    if entries["SHA256SUMS"][1] != 0o644:
        raise BundleError("SHA256SUMS must have mode 0644")

    checksums = parse_checksums(entries["SHA256SUMS"][0])
    expected_checksum_names = {output["output"] for output in resolved_outputs}
    if set(checksums) != expected_checksum_names:
        raise BundleError("SHA256SUMS does not list the exact binary inventory")

    with tempfile.TemporaryDirectory(prefix="rest-command-verify-") as temp:
        temp_root = Path(temp)
        for index, raw_output in enumerate(resolved_outputs):
            name = raw_output["output"]
            binary_bytes, mode = entries[name]
            if mode != 0o755:
                raise BundleError(f"{name} must have mode 0755, got {mode:o}")
            digest = sha256_bytes(binary_bytes)
            if digest != checksums[name] or digest != raw_output["sha256"]:
                raise BundleError(f"{name} checksum does not match its manifest")
            if len(binary_bytes) != raw_output["size"]:
                raise BundleError(f"{name} size does not match its manifest")

            # ``go version -m`` and ``file`` inspect filesystem paths. Use a
            # verifier-owned name so an archive member can never choose where
            # its bytes are written.
            binary = temp_root / f"output-{index}"
            binary.write_bytes(binary_bytes)
            binary.chmod(0o755)
            command = CommandBuild(
                name=raw_output["name"],
                package=raw_output["package"],
                target=expected_target,
                cgo_enabled=raw_output["cgo_enabled"],
                ldflags=tuple(raw_output["ldflags"]),
                archive_path=name,
            )
            validate_file_description(
                raw_output["file_description"], target, command
            )
            inspection = inspect_binary(
                binary,
                repo_root,
                target,
                command,
                command.ldflags,
                expected_candidate,
                raw_output["import_path"],
            )
            validate_recorded_inspection(name, raw_output, inspection)


def validate_recorded_inspection(
    name: str,
    recorded: dict[str, Any],
    inspection: BinaryInspection,
) -> None:
    """Require the resolved build record to match the reopened binary exactly."""

    for field in (
        "file_description",
        "go_version",
        "vcs_modified",
        "vcs_revision",
    ):
        detail = getattr(inspection, field)
        if recorded[field] != detail:
            raise BundleError(
                f"{name} {field} is {recorded[field]!r}, expected {detail!r}"
            )


def build_bundle(args: argparse.Namespace) -> None:
    """Build, package, and independently reopen one target's complete inventory."""

    repo_root = args.repo_root.resolve()
    manifest = load_source_manifest(args.manifest.resolve(), repo_root)
    if args.target not in manifest.targets:
        raise BundleError(f"target is not declared in the manifest: {args.target}")
    target = manifest.targets[args.target]
    target_commands = [
        command for command in manifest.outputs if command.target == args.target
    ]
    timestamp = parse_build_timestamp(args.build_timestamp)
    validate_identity(args.version, args.candidate_sha, args.short_sha)

    # The candidate label is only honest when it names this exact checkout.
    # CI rejects all source changes; local validation can opt in and records
    # that the source was dirty in the resolved manifest.
    actual_sha = command_output(["git", "rev-parse", "HEAD"], repo_root)
    if actual_sha != args.candidate_sha:
        raise BundleError(
            f"checked-out candidate is {actual_sha}, expected {args.candidate_sha}"
        )
    source_status = command_output(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"], repo_root
    )
    source_dirty = bool(source_status)
    if source_dirty and not args.allow_dirty:
        raise BundleError(
            "the repository worktree has tracked or untracked changes anywhere in the "
            f"checkout; refusing to label it as candidate {args.candidate_sha}"
        )
    template_values = {
        "BUILD_TIME_LEGACY": timestamp.strftime("%Y-%m-%d %H:%M:%S"),
        "BUILD_TIME_RFC3339": timestamp.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "SHORT_SHA": args.short_sha,
        "VERSION": args.version,
    }
    go_module_path = module_path(repo_root)
    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    if any(output_dir.iterdir()):
        raise BundleError(f"output directory must be empty: {output_dir}")
    stem = f"rest-command-bundle-{target.name}"

    with tempfile.TemporaryDirectory(prefix="rest-command-build-") as temp:
        temp_root = Path(temp)
        bin_root = temp_root / "bin"
        bin_root.mkdir()
        resolved_outputs: list[dict[str, Any]] = []
        binary_contents: dict[str, bytes] = {}
        for command in target_commands:
            binary = bin_root / command.name
            ldflags = resolve_ldflags(command, template_values)
            go_build_command = ["go", "build", "-o", str(binary)]
            if ldflags:
                go_build_command.extend(["-ldflags", shlex.join(ldflags)])
            go_build_command.append(command.package)
            environment = os.environ.copy()
            environment.update(
                {
                    "CGO_ENABLED": "1" if command.cgo_enabled else "0",
                    "GOARCH": target.goarch,
                    "GOOS": target.goos,
                }
            )
            print(f"Building {command.name} for {target.name}", flush=True)
            try:
                subprocess.run(
                    go_build_command,
                    cwd=repo_root,
                    env=environment,
                    check=True,
                )
            except subprocess.CalledProcessError as error:
                raise BundleError(
                    f"go build failed for {target.name}/{command.name}"
                ) from error
            binary.chmod(0o755)
            import_path = expected_import_path(go_module_path, command.package)
            inspection = inspect_binary(
                binary,
                repo_root,
                target,
                command,
                ldflags,
                args.candidate_sha,
                import_path,
            )
            binary_bytes = binary.read_bytes()
            binary_contents[command.archive_path] = binary_bytes
            resolved_outputs.append(
                {
                    "cgo_enabled": command.cgo_enabled,
                    "file_description": inspection.file_description,
                    "go_version": inspection.go_version,
                    "import_path": import_path,
                    "ldflags": list(ldflags),
                    "name": command.name,
                    "output": command.archive_path,
                    "package": command.package,
                    "sha256": sha256_bytes(binary_bytes),
                    "size": len(binary_bytes),
                    "vcs_modified": inspection.vcs_modified,
                    "vcs_revision": inspection.vcs_revision,
                }
            )

        resolved_manifest = {
            "build_timestamp": args.build_timestamp,
            "candidate_sha": args.candidate_sha,
            "outputs": resolved_outputs,
            "schema_version": SCHEMA_VERSION,
            "short_sha": args.short_sha,
            "source_manifest_sha256": manifest.sha256,
            "source_dirty": source_dirty,
            "target": {
                "goarch": target.goarch,
                "goos": target.goos,
                "name": target.name,
            },
            "version": args.version,
        }
        manifest_bytes = canonical_json(resolved_manifest)
        checksum_bytes = "".join(
            f"{sha256_bytes(binary_contents[name])}  {name}\n"
            for name in sorted(binary_contents)
        ).encode()
        with tempfile.TemporaryDirectory(
            prefix=".rest-command-stage-", dir=output_dir
        ) as stage:
            stage_root = Path(stage)
            staged_bundle = stage_root / f"{stem}.tar.gz"
            staged_checksum = stage_root / f"{stem}.tar.gz.sha256"
            staged_manifest = stage_root / f"{stem}.manifest.json"
            create_bundle(
                staged_bundle,
                manifest_bytes,
                checksum_bytes,
                binary_contents,
                int(timestamp.timestamp()),
            )
            staged_manifest.write_bytes(manifest_bytes)
            staged_checksum.write_text(
                f"{sha256_file(staged_bundle)}  {staged_bundle.name}\n",
                encoding="utf-8",
                newline="\n",
            )

            verify_bundle(
                staged_bundle,
                staged_checksum,
                staged_manifest,
                manifest,
                repo_root,
                target.name,
                args.version,
                args.build_timestamp,
                args.candidate_sha,
                args.short_sha,
                source_dirty,
            )
            # Publish the archive last so a consumer never sees it before its
            # resolved manifest and checksum sidecar are ready.
            final_paths = publish_bundle_files(
                (staged_manifest, staged_checksum, staged_bundle), output_dir
            )

    bundle_path = final_paths[staged_bundle.name]
    print(
        f"Verified {len(target_commands)} outputs in {bundle_path} "
        f"({bundle_path.stat().st_size} bytes)",
        flush=True,
    )


def check_manifest(args: argparse.Namespace) -> None:
    """Validate the checked-in source contract without compiling commands."""

    manifest = load_source_manifest(args.manifest.resolve(), args.repo_root.resolve())
    print(
        f"Validated {len(manifest.outputs)} outputs across "
        f"{len(manifest.targets)} targets"
    )


def verify_existing_bundle(args: argparse.Namespace) -> None:
    """Verify supplied bundle files against the checked-in source contract."""

    validate_identity(args.version, args.candidate_sha, args.short_sha)
    repo_root = args.repo_root.resolve()
    manifest = load_source_manifest(args.manifest.resolve(), repo_root)
    verify_bundle(
        args.bundle.resolve(),
        args.checksum.resolve(),
        args.resolved_manifest.resolve(),
        manifest,
        repo_root,
        args.target,
        args.version,
        args.build_timestamp,
        args.candidate_sha,
        args.short_sha,
        None if args.allow_dirty else False,
    )
    print(f"Verified {args.bundle}")


def parser() -> argparse.ArgumentParser:
    """Define the check, build, and verify command-line contracts."""

    result = argparse.ArgumentParser(
        description="Build and verify exact REST command bundles."
    )
    subparsers = result.add_subparsers(dest="command", required=True)

    check = subparsers.add_parser("check", help="validate the source manifest")
    check.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    check.add_argument("--repo-root", type=Path, default=DEFAULT_REPO_ROOT)
    check.set_defaults(function=check_manifest)

    build = subparsers.add_parser("build", help="build and verify one target bundle")
    build.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    build.add_argument("--repo-root", type=Path, default=DEFAULT_REPO_ROOT)
    build.add_argument("--target", required=True)
    build.add_argument("--output-dir", type=Path, required=True)
    build.add_argument("--version", required=True)
    build.add_argument("--build-timestamp", required=True)
    build.add_argument("--candidate-sha", required=True)
    build.add_argument("--short-sha", required=True)
    build.add_argument(
        "--allow-dirty",
        action="store_true",
        help="allow local validation from a dirty checkout and record that fact",
    )
    build.set_defaults(function=build_bundle)

    verify = subparsers.add_parser("verify", help="verify one existing target bundle")
    verify.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    verify.add_argument("--repo-root", type=Path, default=DEFAULT_REPO_ROOT)
    verify.add_argument("--target", required=True)
    verify.add_argument("--bundle", type=Path, required=True)
    verify.add_argument("--checksum", type=Path, required=True)
    verify.add_argument("--resolved-manifest", type=Path, required=True)
    verify.add_argument("--version", required=True)
    verify.add_argument("--build-timestamp", required=True)
    verify.add_argument("--candidate-sha", required=True)
    verify.add_argument("--short-sha", required=True)
    verify.add_argument(
        "--allow-dirty",
        action="store_true",
        help="allow a bundle whose resolved manifest records a dirty source checkout",
    )
    verify.set_defaults(function=verify_existing_bundle)
    return result


def main() -> int:
    """Run the selected command and present contract failures consistently."""

    args = parser().parse_args()
    try:
        args.function(args)
    except (BundleError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
