#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Install the native CLI versions used by the local deployment workflows."""

import argparse
import hashlib
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile

VERSIONS = {"kind": "v0.32.0", "kubectl": "v1.36.3", "helm": "v3.21.3",
            "tilt": "v0.37.8", "devspace": "v6.3.21"}
DEFAULT_TOOLS = ("kind", "kubectl", "helm", "tilt")


def native_platform():
    system = {"Linux": "linux", "Darwin": "darwin"}.get(platform.system())
    arch = {"x86_64": "amd64", "aarch64": "arm64", "arm64": "arm64"}.get(platform.machine())
    if not system or not arch:
        raise ValueError("supported platforms are Linux/macOS on ARM64/AMD64")
    return system, arch


def artifact(tool, system, arch):
    version = VERSIONS[tool]
    if tool in ("kind", "devspace"):
        project = "kubernetes-sigs/kind" if tool == "kind" else "loft-sh/devspace"
        url = f"https://github.com/{project}/releases/download/{version}/{tool}-{system}-{arch}"
        return url, url + (".sha256sum" if tool == "kind" else ".sha256"), None
    if tool == "kubectl":
        url = f"https://dl.k8s.io/release/{version}/bin/{system}/{arch}/kubectl"
        return url, url + ".sha256", None
    if tool == "helm":
        url = f"https://get.helm.sh/helm-{version}-{system}-{arch}.tar.gz"
        return url, url + ".sha256sum", f"{system}-{arch}/helm"
    tilt_system = "mac" if system == "darwin" else system
    tilt_arch = "x86_64" if arch == "amd64" else arch
    base = f"https://github.com/tilt-dev/tilt/releases/download/{version}"
    return f"{base}/tilt.{version[1:]}.{tilt_system}.{tilt_arch}.tar.gz", f"{base}/checksums.txt", "tilt"


def download(url, destination):
    subprocess.run(["curl", "--fail", "--silent", "--show-error", "--location",
                    "--proto", "=https", "--proto-redir", "=https", "--retry", "3",
                    "--connect-timeout", "30", "--max-time", "600", "--output", str(destination), url], check=True)


def verify(payload, manifest, filename):
    matches = []
    for line in manifest.read_text().splitlines():
        fields = line.split()
        if fields and re.fullmatch(r"[0-9a-fA-F]{64}", fields[0]):
            if len(fields) == 1 or (len(fields) == 2 and Path(fields[1].lstrip("*")).name == filename):
                matches.append(fields[0].lower())
    with payload.open("rb") as stream:
        checksum = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            checksum.update(block)
        actual = checksum.hexdigest()
    if len(matches) != 1 or matches[0] != actual:
        raise ValueError(f"checksum verification failed for {filename}")


def install(prefix, tools):
    system, arch = native_platform()
    prefix = prefix.expanduser().absolute()
    prefix.mkdir(parents=True, exist_ok=True)
    for tool in tools:
        destination = prefix / tool
        if destination.is_symlink() or (destination.exists() and not destination.is_file()):
            raise ValueError(f"refusing nonregular destination: {destination}")
    # Verify the whole selection before replacing any installed binary.
    with tempfile.TemporaryDirectory(prefix=".nico-tools-", dir=prefix) as temporary:
        work = Path(temporary)
        for tool in tools:
            url, checksum_url, member = artifact(tool, system, arch)
            payload, checksum = work / (tool + ".download"), work / (tool + ".sha256")
            download(url, payload)
            download(checksum_url, checksum)
            verify(payload, checksum, url.rsplit("/", 1)[1])
            staged = work / tool
            if member:
                with tarfile.open(payload, "r:gz") as archive:
                    entry = archive.getmember(member)
                    if not entry.isfile():
                        raise ValueError(f"archive member is not a regular file: {member}")
                    with archive.extractfile(entry) as source, staged.open("wb") as output:
                        shutil.copyfileobj(source, output)
            else:
                payload.rename(staged)
            staged.chmod(0o755)
        for tool in tools:
            os.replace(work / tool, prefix / tool)
            print(f"Installed {tool} {VERSIONS[tool]} ({system}/{arch}) at {prefix / tool}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=Path.home() / ".local/bin",
                        help="installation directory (default: ~/.local/bin); selected binaries are replaced")
    parser.add_argument("tools", nargs="*", help="tools to install: kind, kubectl, helm, tilt, devspace; default: kind kubectl helm tilt")
    args = parser.parse_args()
    tools = args.tools or DEFAULT_TOOLS
    if len(set(tools)) != len(tools) or any(tool not in VERSIONS for tool in tools):
        parser.error("select distinct supported tool names")
    try:
        install(args.bin_dir, tools)
    except (OSError, ValueError, KeyError, tarfile.TarError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"install-tools: {error}\n")


if __name__ == "__main__":
    main()
