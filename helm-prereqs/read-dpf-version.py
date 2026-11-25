#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Read the shared DPF version for setup and manual installation commands."""

import re
import sys
from pathlib import Path


VERSION_NUMBER = r"(?:0|[1-9][0-9]*)"
PRERELEASE_ID = rf"(?:{VERSION_NUMBER}|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*)"
IMAGE_VERSION = re.compile(
    rf"{VERSION_NUMBER}\.{VERSION_NUMBER}\.{VERSION_NUMBER}"
    rf"(?:-{PRERELEASE_ID}(?:\.{PRERELEASE_ID})*)?"
)


def read_version(path):
    try:
        import tomllib
    except ImportError:
        try:
            import tomli as tomllib
        except ImportError as error:
            raise ValueError("DPF setup requires Python 3.11 or later, or the tomli package") from error

    with path.open("rb") as manifest:
        version = tomllib.load(manifest).get("version")
    if not isinstance(version, str) or not IMAGE_VERSION.fullmatch(version):
        raise ValueError("DPF version must be a semantic version without a leading v or build metadata")
    return version


if __name__ == "__main__":
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).with_name("dpf-versions.toml")
    try:
        print(read_version(path))
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        sys.exit(1)
