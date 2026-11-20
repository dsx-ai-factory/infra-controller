#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Build real Scout boot artifacts for native KVM guests without GPU/DPU devices."""

import argparse
import json
import os
import platform
import shutil
import subprocess
from pathlib import Path

from boot_build import sha256, stage_base, stage_profile

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]


def profiles(architecture):
    return (f"scout-oss-{architecture}", f"scout-loader-{architecture}")


def vm_config(config):
    # Keep the existing userspace and boot format, but use Ubuntu's 4 KiB
    # virtual kernel in both stages. vfkit does not expose 64 KiB page support.
    lines = config.splitlines()
    if config.count("Packages=") != 1 or not any(
        line.strip().startswith("linux-image-") for line in lines
    ):
        raise ValueError("Scout profile no longer declares its kernel packages")
    prefixes = (
        "linux-image-",
        "linux-headers-",
        "linux-modules-",
        "linux-firmware",
    )
    config = (
        "\n".join(line for line in lines if not line.strip().startswith(prefixes))
        + "\n"
    )
    return config.replace(
        "Packages=", "Packages=\n     linux-image-virtual\n     lsb-release", 1
    )


def stage_profiles(repo, work, package, architecture, ipv6_boot=False):
    pxe = repo / "pxe"
    stage_base(repo, work)
    for profile in profiles(architecture):
        destination = stage_profile(repo, work, profile, ipv6_boot)
        config = destination / "mkosi.conf"
        config.write_text(vm_config(config.read_text()))
        extra = destination / "mkosi.extra"
        (extra / "opt/forge").mkdir(parents=True, exist_ok=True)
        shutil.copy2(pxe / "common_files/forge-scout-network.sh", extra / "opt/forge")
        if profile == f"scout-loader-{architecture}":
            loader = (pxe / "common_files/scout-loader-rclocal").read_text()
            legacy = 'http://carbide-static-pxe.forge/public/blobs/internal/'
            if loader.count(legacy) != 1:
                raise ValueError("Scout loader no longer has the expected rootfs fallback")
            (extra / "etc/rc.local").write_text(loader.replace(
                legacy, ('http://carbide-static-pxe.forge:8080/public/blobs/internal/' if ipv6_boot
                         else 'http://nico-pxe.nico-system.svc.cluster.local/public/blobs/internal/')
            ))
            (extra / "etc/rc.local").chmod(0o755)
        else:
            # GPU driver repositories and post-install actions do not apply to
            # these virtual targets. Preserve the real Scout service and agent.
            if (extra / "etc/apt").exists():
                shutil.rmtree(extra / "etc/apt")
            service = extra / "etc/systemd/system"
            service.mkdir(parents=True, exist_ok=True)
            shutil.copy2(repo / "crates/scout/misc/forge-scout.service", service)
            build = extra / "build-output"
            build.mkdir(exist_ok=True)
            shutil.copy2(package, build / "forge-scout.deb")
            shutil.copy2(pxe / "common_files/load_modules.sh", extra / "opt/forge")
            shutil.copy2(
                HERE / "scout-vm-postinst.sh", destination / "mkosi.postinst.chroot"
            )
            (destination / "mkosi.postinst.chroot").chmod(0o755)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--scout-package",
        type=Path,
        required=True,
        help="real native forge-scout .deb from the revision under test",
    )
    parser.add_argument(
        "--output",
        type=Path,
        required=True,
        help="new or empty directory for Scout payloads and retained mkosi build workspace",
    )
    parser.add_argument('--ipv6-boot', action='store_true',
                        help='use the lab IPv6 DNS/PXE endpoints and DHCP-provided DNS')
    args = parser.parse_args()
    architecture = platform.machine()
    if architecture not in ('aarch64', 'x86_64'):
        parser.error('a native ARM64 or AMD64 runtime is required')
    if platform.system() != "Linux" or os.geteuid() != 0:
        parser.error(
            "run as root on native ARM64 or x86_64 Linux with the repository's mkosi prerequisites installed"
        )
    package = args.scout_package.resolve(strict=True)
    package_architecture = subprocess.check_output(
        ["dpkg-deb", "--field", str(package), "Architecture"], text=True
    ).strip()
    expected = "arm64" if architecture == "aarch64" else "amd64"
    if package_architecture != expected:
        parser.error(f"--scout-package must have Architecture: {expected}")
    version = subprocess.check_output(
        ["dpkg-deb", "--field", str(package), "Version"], text=True
    ).strip()
    mkosi = REPO / "pxe/mkosi/bin/mkosi"
    if not mkosi.is_file():
        parser.error("initialize the pinned pxe/mkosi submodule first")
    output = args.output.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error(
            "--output must be new or empty; prior artifacts are never overwritten"
        )
    output.mkdir(parents=True, exist_ok=True)
    work = output / "build"
    work.mkdir()
    stage_profiles(REPO, work, package, architecture, args.ipv6_boot)
    built = work / "output"
    for profile in profiles(architecture):
        subprocess.run(
            [
                str(mkosi),
                "-E",
                f"PKG_VERSION={version}-vm",
                "--profile",
                profile,
                f"--output-dir={built}",
                f"--package-cache-directory={work / 'package-cache'}",
                "build",
            ],
            cwd=work,
            check=True,
        )
    payload = output / architecture
    payload.mkdir()
    shutil.copy2(built / "loader.efi", payload / "scout.efi")
    shutil.copy2(built / "scout.squashfs", payload / "scout.squashfs")
    for artifact in payload.iterdir():
        artifact.chmod(0o644)
    (payload / "scout.network-profile").write_text("ipv6\n" if args.ipv6_boot else "default\n")
    (output / "build-info.json").write_text(
        json.dumps(
            {
                "scout_package_version": version,
                "architecture": architecture,
                "kernel_package": "linux-image-virtual",
                "ipv6_boot": args.ipv6_boot,
                "payload_sha256": {
                    name: sha256(payload / name)
                    for name in ("scout.efi", "scout.squashfs")
                },
                "scout_package_sha256": sha256(package),
            },
            indent=2,
        )
        + "\n"
    )
    (output / ".dockerignore").write_text("build\n")
    print(f"Boot payloads: {payload}")


if __name__ == "__main__":
    main()
