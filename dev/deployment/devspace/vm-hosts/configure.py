#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Render only the Helm values that differ for real zero-DPU guests."""

import argparse
import json
import re
from pathlib import Path

import yaml

import lab


def values(base, image, architecture=None):
    if not image or not re.fullmatch(r"[a-zA-Z0-9._/:@-]+", image):
        raise ValueError(
            "LOCAL_DEV_VM_HOSTS_BOOT_IMAGE must name a boot-artifact container image"
        )
    architecture = architecture or lab.native_architecture()
    if architecture not in ("aarch64", "x86_64"):
        raise ValueError(f"unsupported architecture: {architecture}")
    site = base["nico-api"]["siteConfig"]["nicoApiSiteConfig"]
    site = "tpm_required = false\n" + site
    site = re.sub(r"(?m)^bmc_proxy = .*\n", "", site)
    site = site.replace(
        "allow_machineatron_scout_stream = true",
        "allow_machineatron_scout_stream = false",
    )
    site = site.replace(
        "allow_insecure_discovery = true", "allow_insecure_discovery = false"
    )
    site = site.replace("permissive_mode = true", "permissive_mode = false")
    site = site.replace("skip_polling_checks = true", "skip_polling_checks = false")
    site = site.replace("autoupdate = true", "autoupdate = false")
    # These comments describe the removed MAT configuration, not real hosts.
    site = re.sub(
        r"(?m)^# devspace.*?^allow_insecure_discovery",
        "allow_insecure_discovery",
        site,
        flags=re.S,
    )
    site = site.replace(
        "# Isolated simulator deployment: MAT may claim any Scout machine ID.\n", ""
    )
    proxy = re.sub(
        r"(?m)^bmc_proxy = .*\n",
        "",
        base["nico-bmc-proxy"]["configFiles"]["nicoBmcProxyConfig"],
    )
    return {
        "nico-api": {"siteConfig": {"nicoApiSiteConfig": site}},
        "nico-bmc-proxy": {"configFiles": {"nicoBmcProxyConfig": proxy}},
        "nico-machine-a-tron": {"enabled": False},
        "nico-dhcp": {
            "dhcp": {"v6Enabled": True, "v6ServerIdentifier": "020000840001"}
        },
        "nico-pxe": {
            "bootArtifactContainers": [
                {
                    "name": "vm-hosts-boot-artifacts",
                    "image": image,
                    "imagePullPolicy": "IfNotPresent",
                    "command": [
                        "sh",
                        "-ec",
                        f"test -s /{architecture}/ipxe.efi; test -s /{architecture}/scout.efi; test -s /{architecture}/scout.squashfs; test -s /{architecture}/qcow-imager.efi; cp -r /{architecture} /forge-boot-artifacts/blobs/internal/",
                    ],
                }
            ]
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--boot-image", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    base = yaml.safe_load(
        (Path(__file__).resolve().parent.parent / "values.base.yaml").read_text()
    )
    rendered = values(base, args.boot_image)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(rendered, indent=2) + "\n")


if __name__ == "__main__":
    main()
