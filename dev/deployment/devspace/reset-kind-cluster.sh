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

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

log() {
  printf '[local-dev] %s\n' "$*"
}

require_bin() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'missing required binary: %s\n' "$1" >&2
    exit 1
  }
}

main() {
  require_bin docker
  require_bin helm
  require_bin kind
  require_bin kubectl
  require_bin base64

  local current_context cluster_name control_plane node_image
  current_context="$(kubectl config current-context 2>/dev/null || true)"
  case "${current_context}" in
    kind-*) cluster_name="${current_context#kind-}" ;;
    *)
      printf 'purge requires a kind context; current context is %s\n' \
        "${current_context:-unset}" >&2
      exit 1
      ;;
  esac

  control_plane="${cluster_name}-control-plane"
  node_image="$(docker inspect --format '{{.Config.Image}}' "${control_plane}" 2>/dev/null || true)"
  if [[ -z "${node_image}" ]]; then
    printf 'could not determine the node image from container %s\n' \
      "${control_plane}" >&2
    exit 1
  fi

  log "Deleting kind cluster ${cluster_name}"
  kind delete cluster --name "${cluster_name}"

  log "Recreating kind cluster ${cluster_name} with ${node_image}"
  kind create cluster --name "${cluster_name}" --image "${node_image}"

  log "Installing clean local prerequisites"
  "${SCRIPT_DIR}/bootstrap-prereqs.sh"

  log "Reset complete; deploy with devspace deploy --skip-build"
}

main "$@"
