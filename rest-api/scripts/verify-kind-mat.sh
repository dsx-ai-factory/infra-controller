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

# verify-kind-mat.sh — Verify machine-a-tron registered its BMC Services.
#
# A MAT BMC Service is a Service created by mat-k8s-controller
# (app.kubernetes.io/managed-by=mat-k8s-controller) with non-empty
# externalIPs. Services that merely carry externalIPs — the Kind DNS/PXE
# VIP Services — do not count: their presence proves a DNS/PXE deployment,
# not MAT registration with Core.
#
# Modes:
#   (default)      Poll the cluster until at least one MAT BMC Service
#                  appears; fail after MAT_ATTEMPTS polls of MAT_INTERVAL
#                  seconds. Registration is asynchronous: machine-a-tron
#                  must discover its mock BMCs and register them with Core.
#   --from-file F  One selection pass against a `kubectl get svc -o json`
#                  payload; exits 0 only if it contains a MAT BMC Service.
#   --self-test    Run the built-in fixtures (empty, DNS/PXE-only, and
#                  successful MAT registration) without a cluster; exits
#                  nonzero if any case misbehaves.

set -euo pipefail

NAMESPACE="${NAMESPACE:-nico-system}"
MAT_ATTEMPTS="${MAT_ATTEMPTS:-30}"
MAT_INTERVAL="${MAT_INTERVAL:-10}"

# Select MAT BMC Services: created by mat-k8s-controller, carrying a BMC
# machine type (host, dpu, or pod-bmc — NVOS switch Services are excluded),
# and exposing externalIPs. Prints one Service name per line.
FILTER='.items[]
  | select(.metadata.labels["app.kubernetes.io/managed-by"] == "mat-k8s-controller")
  | select(.metadata.labels["nvidia-infra-controller/mat-machine-type"] | IN("host", "dpu", "pod-bmc"))
  | select((.spec.externalIPs // []) | length > 0)
  | .metadata.name'

# mat_bmc_services PAYLOAD_FILE — prints the MAT BMC Service names contained
# in the given `kubectl get svc -o json` payload file, or queries the cluster
# when no file is given.
mat_bmc_services() {
  if [ -n "${1:-}" ]; then
    jq -r "$FILTER" "$1"
  else
    kubectl -n "$NAMESPACE" get svc -o json | jq -r "$FILTER"
  fi
}

# check_payload PAYLOAD_FILE — succeeds and lists the names when the payload
# contains at least one MAT BMC Service.
check_payload() {
  local names
  names=$(mat_bmc_services "$1")
  if [ -n "$names" ]; then
    echo "$names" | sed 's/^/  /'
    return 0
  fi
  return 1
}

self_test() {
  local fixtures_dir failures=0 f fixture
  fixtures_dir="$(cd "$(dirname "$0")" && pwd)/testdata/kind-verify-mat"

  # A missing or unreadable fixture would make check_payload fail and be
  # mistaken for a correct rejection, silently skipping the case; validate
  # the fixture itself first so the selector is actually exercised.
  readable_fixture() {
    [ -r "$1" ] && jq -e . "$1" >/dev/null 2>&1
  }

  for f in empty dns-pxe-only nvos-only; do
    fixture="$fixtures_dir/$f.json"
    if ! readable_fixture "$fixture"; then
      echo "self-test: cannot read fixture $fixture" >&2
      failures=1
      continue
    fi
    if check_payload "$fixture" >/dev/null; then
      echo "self-test: $f payload should have been rejected" >&2
      failures=1
    else
      echo "self-test: $f payload correctly rejected"
    fi
  done

  for f in mat-registered; do
    fixture="$fixtures_dir/$f.json"
    if ! readable_fixture "$fixture"; then
      echo "self-test: cannot read fixture $fixture" >&2
      failures=1
      continue
    fi
    if check_payload "$fixture" >/dev/null; then
      echo "self-test: $f payload correctly accepted"
    else
      echo "self-test: $f payload should have been accepted" >&2
      failures=1
    fi
  done

  if [ "$failures" -ne 0 ]; then
    echo "ERROR: verify-kind-mat self-test failed" >&2
    exit 1
  fi
  echo "verify-kind-mat self-test: OK"
}

case "${1:-}" in
  --self-test)
    self_test
    ;;
  --from-file)
    [ $# -eq 2 ] || { echo "usage: $0 --from-file <svc-json-file>" >&2; exit 2; }
    if check_payload "$2"; then
      echo "MAT BMC Service found in $2"
    else
      echo "ERROR: no MAT BMC Service (managed-by=mat-k8s-controller with externalIPs) in $2" >&2
      exit 1
    fi
    ;;
  ""|*)
    echo "Waiting for MAT BMC Services in ${NAMESPACE} (managed by mat-k8s-controller, with externalIPs)..."
    for attempt in $(seq 1 "$MAT_ATTEMPTS"); do
      # Transient kubectl/jq failures during cluster startup are retried,
      # not fatal; a persistent failure exhausts the attempts below.
      names=$(mat_bmc_services "" 2>/dev/null || true)
      if [ -n "$names" ]; then
        echo "MAT BMC Services registered:"
        echo "$names" | sed 's/^/  /'
        echo "machine-a-tron verify: OK — MAT BMC Services with externalIPs found"
        exit 0
      fi
      echo "  attempt ${attempt}/${MAT_ATTEMPTS}: no MAT BMC Services yet; retrying in ${MAT_INTERVAL}s..." >&2
      sleep "$MAT_INTERVAL"
    done
    echo "ERROR: no MAT BMC Service (managed-by=mat-k8s-controller with externalIPs) appeared within $((MAT_ATTEMPTS * MAT_INTERVAL))s" >&2
    echo "machine-a-tron may have failed to register with Core; check the mat-k8s-controller logs." >&2
    exit 1
    ;;
esac
