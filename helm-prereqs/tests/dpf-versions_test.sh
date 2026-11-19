#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image_tag_defaults="$(sed -n '/^NICO_DPF_IMAGE_TAG=/,/^fi/p' "${SCRIPT_DIR}/setup.sh")"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

version="$(python3 "${SCRIPT_DIR}/read-dpf-version.py")"
printf '%s\n' 'version = "30.0.0-rc.4"' > "${WORK}/prerelease.toml"
[[ "$(python3 "${SCRIPT_DIR}/read-dpf-version.py" "${WORK}/prerelease.toml")" == 30.0.0-rc.4 ]]
for invalid_manifest in 'version = 30' 'version = ""' 'version = [' \
    'version = "v30.0.0"' 'version = "30.0"' 'version = "30.0.0+build"'; do
    printf '%s\n' "${invalid_manifest}" > "${WORK}/invalid.toml"
    if python3 "${SCRIPT_DIR}/read-dpf-version.py" "${WORK}/invalid.toml" 2>/dev/null; then
        echo "an invalid TOML version must be rejected" >&2
        exit 1
    fi
done
INSTALL_DPF=true
NICO_DPF_IMAGE_TAG=""
eval "${image_tag_defaults}"
[[ "${NICO_DPF_IMAGE_TAG}" == "v${version}" ]]

# Explicit image tags and skipped DPF installations do not need a TOML parser.
python3() { return 1; }
NICO_DPF_IMAGE_TAG="custom-image"
eval "${image_tag_defaults}"
[[ "${NICO_DPF_IMAGE_TAG}" == custom-image ]]
INSTALL_DPF=false
NICO_DPF_IMAGE_TAG=""
eval "${image_tag_defaults}"
[[ -z "${NICO_DPF_IMAGE_TAG}" ]]
unset -f python3

echo "DPF version selection tests passed"
