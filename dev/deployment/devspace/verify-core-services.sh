#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

namespace="${1:-nico-system}"
timeout_command="$(command -v timeout || command -v gtimeout)" || {
  printf 'Core verification requires GNU coreutils timeout (gtimeout on macOS)\n' >&2
  exit 1
}
workloads="$(kubectl -n "${namespace}" get deployments,statefulsets,daemonsets -o name)"
if [[ -z "${workloads}" ]]; then
  printf 'No Core workloads found in %s\n' "${namespace}" >&2
  exit 1
fi
while IFS= read -r workload; do
  kubectl -n "${namespace}" rollout status "${workload}" --timeout=300s
done <<<"${workloads}"

# Services without readiness probes can briefly report Ready before crashing.
# Require a stable pod population and restart counts as well as readiness.
previous=''
for attempt in {1..3}; do
  pods="$(kubectl -n "${namespace}" get pods -o json |
    jq '.items |= map(select(.metadata.deletionTimestamp == null))')"
  if ! jq -e '.items | length > 0 and all(.[];
      .status.phase == "Succeeded" or
      (.status.phase == "Running" and any(.status.conditions[]?;
        .type == "Ready" and .status == "True")))' <<<"${pods}" >/dev/null; then
    printf 'Core pods in %s are not all Ready\n' "${namespace}" >&2
    exit 1
  fi
  current="$(jq -c '[.items[] | select(.status.phase != "Succeeded") |
    {uid: .metadata.uid, restarts: [.status.containerStatuses[]?.restartCount]}] |
    sort_by(.uid)' <<<"${pods}")"
  if [[ -n "${previous}" && "${current}" != "${previous}" ]]; then
    printf 'Core pods restarted or were replaced during health verification\n' >&2
    exit 1
  fi
  previous="${current}"
  if [[ "${attempt}" != 3 ]]; then sleep 10; fi
done
printf 'Core workloads Ready with stable restart counts\n'

# MAT requests boot instructions over gRPC; it does not exercise PXE HTTP delivery.
script_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
artifact="scout-firmware-scripts/nvidia/dgxh100/cx7/metadata.toml"
pxe_port="$(kubectl -n "${namespace}" get service nico-pxe -o json |
  jq -er '.spec.ports[] | select(.name == "http") | .port')"
download="$(mktemp)"
trap 'rm -f "${download}"' EXIT
"${timeout_command}" --kill-after=5s 45s \
  kubectl -n "${namespace}" exec deployment/nico-api -c nico-api -- \
  curl --fail --silent --show-error --connect-timeout 10 --max-time 30 \
    "http://nico-pxe.${namespace}.svc.cluster.local.:${pxe_port}/public/${artifact}" >"${download}"
if ! cmp -s "${script_dir}/../../../pxe/${artifact}" "${download}"; then
  printf 'PXE HTTP response does not match the packaged firmware metadata\n' >&2
  exit 1
fi
printf 'PXE HTTP delivery verified against packaged firmware metadata\n'
