#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# CI's amd64 release image is already built and loaded. Use isolated, pinned
# tooling to exercise its real PXE server without another Rust build.
set -euo pipefail
: "${PXE_TEST_IMAGE:?Set PXE_TEST_IMAGE to the locally loaded release image}"
[[ "$(uname -s)-$(uname -m)" == Linux-x86_64 ]]
tools_dir="$(mktemp -d)"
trap 'rm -rf "$tools_dir"' EXIT
curl --fail --silent --show-error --location --retry 3 --max-time 120 \
  https://kind.sigs.k8s.io/dl/v0.30.0/kind-linux-amd64 -o "${tools_dir}/kind"
curl --fail --silent --show-error --location --retry 3 --max-time 120 \
  https://get.helm.sh/helm-v3.19.0-linux-amd64.tar.gz -o "${tools_dir}/helm.tar.gz"
curl --fail --silent --show-error --location --retry 3 --max-time 120 \
  https://dl.k8s.io/release/v1.34.0/bin/linux/amd64/kubectl -o "${tools_dir}/kubectl"
(
  cd "$tools_dir"
  sha256sum --check <<'CHECKSUMS'
517ab7fc89ddeed5fa65abf71530d90648d9638ef0c4cde22c2c11f8097b8889  kind
a7f81ce08007091b86d8bd696eb4d86b8d0f2e1b9f6c714be62f82f96a594496  helm.tar.gz
cfda68cba5848bc3b6c6135ae2f20ba2c78de20059f68789c090166d6abc3e2c  kubectl
CHECKSUMS
  tar -xzf helm.tar.gz linux-amd64/helm
  mv linux-amd64/helm helm
  chmod +x kind helm kubectl
)
export PATH="${tools_dir}:${PATH}"
bash helm/tests/runtime/test-pxe-boot-artifacts.sh
