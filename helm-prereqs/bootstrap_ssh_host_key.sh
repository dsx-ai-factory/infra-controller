#!/usr/bin/env bash
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

# =============================================================================
# bootstrap_ssh_host_key.sh — pre-create ssh-host-key in OpenSSH format
#
# ssh-console-rs requires the host key in OpenSSH PEM format:
#   "-----BEGIN OPENSSH PRIVATE KEY-----"
#
# Helm's genPrivateKey "ed25519" produces PKCS8 PEM format:
#   "-----BEGIN PRIVATE KEY-----"
# which ssh-console-rs rejects with an encoding error at startup.
#
# This script creates the secret before `helmfile sync -l name=nico-prereqs`
# runs. Helm's lookup in templates/_helpers.tpl finds the existing secret and
# reuses the key, so it is never overwritten with a PKCS8-format key.
#
# Idempotent: skips creation if the secret already exists.
#
# Requires: kubectl, ssh-keygen
# =============================================================================
set -euo pipefail

NAMESPACE="${1:-nico-system}"

if kubectl get secret ssh-host-key -n "${NAMESPACE}" &>/dev/null; then
    echo "ssh-host-key already exists in ${NAMESPACE} — skipping"
    exit 0
fi

echo "Generating ssh-host-key in OpenSSH format for ${NAMESPACE}..."
ssh-keygen -t ed25519 -N "" -f /tmp/ssh_host_ed25519_key -C "" -q

kubectl create secret generic ssh-host-key \
    --namespace "${NAMESPACE}" \
    --from-file=ssh_host_ed25519_key=/tmp/ssh_host_ed25519_key \
    --from-file=ssh_host_ed25519_key_pub=/tmp/ssh_host_ed25519_key.pub

kubectl label secret ssh-host-key -n "${NAMESPACE}" \
    app.kubernetes.io/managed-by=Helm --overwrite
kubectl annotate secret ssh-host-key -n "${NAMESPACE}" \
    meta.helm.sh/release-name=nico-prereqs \
    meta.helm.sh/release-namespace=nico-system \
    --overwrite

rm -f /tmp/ssh_host_ed25519_key /tmp/ssh_host_ed25519_key.pub
echo "ssh-host-key created in ${NAMESPACE}"
