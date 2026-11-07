#!/bin/bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# Build script for carbide-dpf-api-harness against GLIBC 2.34
# This script builds carbide-dpf-api-harness using Ubuntu 22.04 (which has GLIBC 2.34)
# Automatically extracts the binary and verifies the build

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
DPF_CRATE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
OUTPUT_BINARY="${DPF_CRATE_DIR}/carbide-dpf-api-harness"
IMAGE_NAME="carbide-dpf-api-harness:glibc2.34"
CONTAINER_NAME="carbide-dpf-api-harness-temp-$$"

cd "${WORKSPACE_ROOT}"

echo "Building carbide-dpf-api-harness against GLIBC 2.34..."
echo "Workspace root: ${WORKSPACE_ROOT}"
echo ""

# Build the Docker image with cache mounts for dependencies
echo "Building Docker image (this may take a while on first run)..."
docker build \
    --progress=plain \
    -f crates/dpf/dev/Dockerfile.carbide-dpf-api-harness-glibc2.34 \
    -t "${IMAGE_NAME}" \
    .

echo ""
echo "Build complete!"
echo ""

# Extract the binary
echo "Extracting binary..."
docker create --name "${CONTAINER_NAME}" "${IMAGE_NAME}" > /dev/null
docker cp "${CONTAINER_NAME}:/usr/local/bin/carbide-dpf-api-harness" "${OUTPUT_BINARY}"
docker rm "${CONTAINER_NAME}" > /dev/null

chmod +x "${OUTPUT_BINARY}"

echo "Binary extracted to: ${OUTPUT_BINARY}"
echo ""

# Verify GLIBC version
echo "Verifying GLIBC version..."
if strings "${OUTPUT_BINARY}" | grep -q "GLIBC_2.34"; then
    echo "Binary is linked against GLIBC 2.34"
else
    echo "WARNING: Could not verify GLIBC 2.34 linkage"
fi

# Check GLIBC version requirements
GLIBC_VERSIONS=$(strings "${OUTPUT_BINARY}" | grep "^GLIBC_" | sort -V | tail -1)
if [ -n "${GLIBC_VERSIONS}" ]; then
    echo "  Highest GLIBC version required: ${GLIBC_VERSIONS}"
    if echo "${GLIBC_VERSIONS}" | grep -q "GLIBC_2.34"; then
        echo "GLIBC 2.34 requirement confirmed"
    fi
fi

# Verify binary is executable and shows help
echo ""
echo "Verifying binary..."
if "${OUTPUT_BINARY}" --help > /dev/null 2>&1; then
    echo "Binary is executable and responds to --help"
else
    echo "WARNING: Binary may not be working correctly"
fi

echo ""
echo "Build and verification complete!"
echo "Binary location: ${OUTPUT_BINARY}"
