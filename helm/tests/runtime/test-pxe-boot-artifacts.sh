#!/usr/bin/env bash

# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Exercise the real PXE server through the rendered chart in a disposable kind
# cluster. PXE_TEST_IMAGE must contain the PXE binary, templates, and coreutils.
set -euo pipefail

: "${PXE_TEST_IMAGE:?Set PXE_TEST_IMAGE to a locally built PXE runtime image}"
repo_root="$(git rev-parse --show-toplevel)"
chart="${PXE_TEST_CHART:-${repo_root}/helm/charts/nico-pxe}"
test_dir="$(mktemp -d)"
cluster="pxe-artifacts-$$"
fixture_tag="$(date +%s)-$$"
fixture_image="pxe-boot-artifacts-test:${fixture_tag}"
export KUBECONFIG="${test_dir}/kubeconfig"
port_forward_pid=""
cluster_created=false

cleanup() {
  result=$?
  if [[ "$result" -ne 0 && "$cluster_created" == true ]]; then
    kubectl get pods -n pxe-test -o wide || true
    kubectl logs -n pxe-test deployment/nico-pxe --all-containers || true
  fi
  if [[ -n "$port_forward_pid" ]]; then
    kill "$port_forward_pid" 2>/dev/null || true
    wait "$port_forward_pid" 2>/dev/null || true
  fi
  if [[ "$cluster_created" == true ]]; then
    kind delete cluster --name "$cluster" >/dev/null
  fi
  docker image rm "$fixture_image" >/dev/null 2>&1 || true
  rm -rf "$test_dir"
}
trap cleanup EXIT

cat >"${test_dir}/Dockerfile" <<'DOCKERFILE'
ARG PXE_TEST_IMAGE
FROM ${PXE_TEST_IMAGE}
USER 0:0
RUN mkdir -p /x86_64 /aarch64 /apt /machine-validation /forge-boot-artifacts/blobs/internal/x86_64 \
    && printf 'restricted artifact\n' >/x86_64/restricted.bin \
    && printf 'legacy artifact\n' >/x86_64/qcow-imaging-test \
    && chmod 0700 /x86_64 && chmod 0600 /x86_64/restricted.bin \
    && printf 'bundled artifact\n' >/forge-boot-artifacts/blobs/internal/x86_64/bundled.bin \
    && chmod 0644 /forge-boot-artifacts/blobs/internal/x86_64/bundled.bin
DOCKERFILE
docker build --build-arg "PXE_TEST_IMAGE=${PXE_TEST_IMAGE}" \
  -t "$fixture_image" "$test_dir"
cluster_created=true
kind create cluster --name "$cluster" --kubeconfig "$KUBECONFIG" \
  --image "${PXE_TEST_NODE_IMAGE:-kindest/node:v1.34.0}" --wait 120s
kind load docker-image --name "$cluster" "$fixture_image"
kubectl create namespace pxe-test
# Static file serving does not call Core or read the client certificate. Supply
# the chart's required Secret without introducing cert-manager or a live API.
kubectl create secret generic nico-pxe-certificate -n pxe-test \
  --from-literal=ca.crt=runtime-test --from-literal=tls.crt=runtime-test \
  --from-literal=tls.key=runtime-test

for scenario in copied-restrictive bundled kustomize; do
  cat >"${test_dir}/values.yaml" <<VALUES
image:
  repository: pxe-boot-artifacts-test
  tag: "${fixture_tag}"
  pullPolicy: Never
env:
  PXE_BIND_ADDRESS: "0.0.0.0"
VALUES
  if [[ "$scenario" == copied-restrictive ]]; then
    cat >>"${test_dir}/values.yaml" <<VALUES
bootArtifacts:
  servePath: /custom-boot-path
bootArtifactContainers:
  - name: copy-artifacts
    image: ${fixture_image}
    imagePullPolicy: Never
    command: ["sh", "-ec", "cp -r /x86_64 /forge-boot-artifacts/blobs/internal"]
initContainers:
  - name: customize-artifacts
    image: ${fixture_image}
    imagePullPolicy: Never
    command: ["sh", "-ec", "chmod 0600 /forge-boot-artifacts/blobs/internal/x86_64/restricted.bin"]
    volumeMounts:
      - name: boot-artifacts
        mountPath: /forge-boot-artifacts/blobs/internal
VALUES
    artifact=restricted.bin
    expected="restricted artifact"
  elif [[ "$scenario" == bundled ]]; then
    artifact=bundled.bin
    expected="bundled artifact"
  else
    artifact=restricted.bin
    expected="restricted artifact"
  fi
  if [[ "$scenario" == kustomize ]]; then
    # Exercise the real optional ConfigMap and the component's four copy sidecars.
    kubectl kustomize "${repo_root}/deploy/tests/pxe-with-boot-artifacts" \
      | kubectl patch --local -f - --type=merge --patch '{}' -o json \
      | jq --slurp --arg image "$fixture_image" '
          map(select(.kind == "Deployment" and .metadata.name == "nico-pxe"))[0]
          | .metadata.namespace = "pxe-test"
          | .spec.template.spec.containers |= map(.image = $image | .imagePullPolicy = "Never")
          | (.spec.template.spec.containers[] | select(.name == "nico-pxe")).env += [{name: "PXE_BIND_ADDRESS", value: "0.0.0.0"}]
        ' | kubectl apply -f -
  else
    helm template pxe-test "$chart" -n pxe-test -f "${test_dir}/values.yaml" \
      --show-only templates/deployment.yaml --show-only templates/rbac.yaml \
      | kubectl apply -f -
  fi
  kubectl rollout status deployment/nico-pxe -n pxe-test --timeout=120s
  [[ "$(kubectl exec -n pxe-test deployment/nico-pxe -c nico-pxe -- id -u)" == 10001 ]]
  kubectl port-forward -n pxe-test deployment/nico-pxe :8080 >"${test_dir}/port-forward.log" 2>&1 &
  port_forward_pid=$!
  port=""
  for ((attempt=0; attempt<60; attempt++)); do
    port="$(sed -n 's/Forwarding from 127.0.0.1:\([0-9]*\).*/\1/p' "${test_dir}/port-forward.log" | head -1)"
    if [[ -n "$port" ]] && curl --silent --fail --max-time 3 "http://127.0.0.1:${port}/metrics" >/dev/null; then
      break
    fi
    sleep 1
  done
  [[ -n "$port" ]]
  code="$(curl --silent --show-error --max-time 10 -o "${test_dir}/body" \
    --write-out '%{http_code}' "http://127.0.0.1:${port}/public/blobs/internal/x86_64/${artifact}")"
  printf '%s: HTTP %s\n' "$scenario" "$code"
  [[ "$code" == 200 && "$(cat "${test_dir}/body")" == "$expected" ]]
  if [[ "$scenario" != bundled ]]; then
    serve_path=/forge-boot-artifacts
    [[ "$scenario" != copied-restrictive ]] || serve_path=/custom-boot-path
    [[ "$(kubectl exec -n pxe-test deployment/nico-pxe -c nico-pxe -- \
      stat -c '%a %u %g' "${serve_path}/blobs/internal/x86_64/restricted.bin")" == '640 0 10001' ]]
    directory_stat="$(kubectl exec -n pxe-test deployment/nico-pxe -c nico-pxe -- \
      stat -c '%a %u %g' "${serve_path}/blobs/internal/x86_64")"
    if [[ "$scenario" == copied-restrictive ]]; then
      # fsGroup sets setgid on volume directories; copied directories can inherit it.
      [[ "$directory_stat" == '750 0 10001' || "$directory_stat" == '2750 0 10001' ]]
    else
      # The legacy Kustomize copier can create the destination with mkdir's 0755
      # before cp runs. It must still normalize the directory's group ownership.
      [[ "${directory_stat#* }" == '0 10001' ]]
    fi
  fi
  printf 'PASS %s: HTTP %s, exact artifact content, serving UID 10001\n' "$scenario" "$code"
  kill "$port_forward_pid"
  wait "$port_forward_pid" 2>/dev/null || true
  port_forward_pid=""
  kubectl delete deployment/nico-pxe -n pxe-test --wait=true
done
