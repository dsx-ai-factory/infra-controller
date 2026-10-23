#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

CLUSTER_NAME=nico-tilt
IP_FAMILY=ipv4
# Paired with kind 0.32.0; the release index supports both native architectures.
NODE_IMAGE=kindest/node:v1.36.1@sha256:3489c7674813ba5d8b1a9977baea8a6e553784dab7b84759d1014dbd78f7ebd5
KUBECONFIG_PATH=""

usage() {
  cat <<'EOF'
Usage: setup-kind.sh [options]

Create or reuse one local kind control-plane node, without deploying NICo.
Run on macOS or Linux with a local Linux AMD64 or ARM64 Docker runtime.

  --name NAME         Cluster name (default: nico-tilt).
  --ip-family FAMILY  ipv4 (default) or dual. Existing clusters must match.
  --image IMAGE       Node image for new clusters (default: pinned kind 0.32.0
                      Kubernetes 1.36.1 image). Does not upgrade existing nodes.
  --kubeconfig PATH   Export after validation (default: ~/.kube/kind-NAME.yaml).
                      An explicitly selected existing file is merged by kind.
  -h, --help          Show this help.

Requires Docker, kind >= 0.32.0, kubectl and jq on PATH. Uses the selected local
Docker Unix socket; remote daemons are rejected. Inherited KUBECONFIG is ignored.
IPv6 requires kernel support, not an upstream IPv6 route. Kind cleans up failed
creation; this command never deletes or recreates preexisting nodes.
EOF
}

die() {
  printf '[setup-kind] %s\n' "$*" >&2
  exit 1
}

while (($#)); do
  case "$1" in
    --name|--ip-family|--image|--kubeconfig)
      [[ $# -ge 2 && -n "$2" && "$2" != -* ]] || die "$1 requires a value"
      case "$1" in
        --name) CLUSTER_NAME="$2" ;;
        --ip-family) IP_FAMILY="$2" ;;
        --image) NODE_IMAGE="$2" ;;
        --kubeconfig) KUBECONFIG_PATH="$2" ;;
      esac
      shift 2
      ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

[[ "$CLUSTER_NAME" =~ ^[a-z0-9]([-a-z0-9]*[a-z0-9])?$ && ${#CLUSTER_NAME} -le 49 ]] ||
  die "--name must be a lowercase DNS label of 1 to 49 characters"
case "$IP_FAMILY" in ipv4|dual) ;; *) die "--ip-family must be ipv4 or dual" ;; esac
host_os="$(uname -s)"
case "$host_os" in Linux|Darwin) ;; *) die "requires a macOS or Linux client" ;; esac
for tool in docker kind kubectl jq; do
  command -v "$tool" >/dev/null 2>&1 || die "missing required command: $tool"
done
kind_version="$(kind version)"
[[ "$kind_version" =~ ^kind\ v([0-9]+)\.([0-9]+)\.([0-9]+) ]] || die "unrecognized kind version: $kind_version"
((10#${BASH_REMATCH[1]} > 0 || 10#${BASH_REMATCH[2]} >= 32)) || die "kind 0.32.0 or newer is required"

# Keep kind's Docker CLI calls on the same daemon inspected below.
docker_context="$(docker context show)"
export DOCKER_CONTEXT="$docker_context" KIND_EXPERIMENTAL_PROVIDER=docker
endpoint="$(docker context inspect "$docker_context" --format '{{.Endpoints.docker.Host}}')"
if [[ "$docker_context" == default && -n "${DOCKER_HOST:-}" ]]; then
  endpoint="$DOCKER_HOST"
fi
[[ "$endpoint" == unix://* ]] || die "a local Docker Unix socket is required; got $endpoint"
platform="$(docker info --format '{{.OSType}}/{{.Architecture}}')"
case "$platform" in
  linux/amd64|linux/x86_64) arch=amd64 ;;
  linux/arm64|linux/aarch64) arch=arm64 ;;
  *) die "unsupported Docker daemon platform: $platform" ;;
esac
# macOS runtimes own the Linux daemon; the client can also run under Rosetta.
if [[ "$host_os" == Linux ]]; then
  case "$(uname -m)" in
    x86_64|amd64) host_arch=amd64 ;;
    aarch64|arm64) host_arch=arm64 ;;
    *) die "requires a native AMD64 or ARM64 Linux host" ;;
  esac
  [[ "$arch" == "$host_arch" ]] || die "Docker architecture $arch does not match host $host_arch"
fi
export DOCKER_DEFAULT_PLATFORM="linux/$arch"

KUBECONFIG_PATH="${KUBECONFIG_PATH:-${HOME}/.kube/kind-${CLUSTER_NAME}.yaml}"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/nico-kind.XXXXXXXX")"
trap 'rm -rf -- "$work_dir"' EXIT
export KUBECONFIG="$work_dir/kubeconfig"
node="${CLUSTER_NAME}-control-plane"
context="kind-${CLUSTER_NAME}"

clusters="$(kind get clusters)"
if grep -Fxq "$CLUSTER_NAME" <<<"$clusters"; then
  printf '[setup-kind] Reusing cluster %s\n' "$CLUSTER_NAME"
else
  docker pull --platform "linux/$arch" "$NODE_IMAGE"
  printf '{"kind":"Cluster","apiVersion":"kind.x-k8s.io/v1alpha4","networking":{"ipFamily":"%s","apiServerAddress":"127.0.0.1"}}\n' \
    "$IP_FAMILY" >"$work_dir/kind.json"
  kind create cluster --name "$CLUSTER_NAME" --image "$NODE_IMAGE" \
    --config "$work_dir/kind.json" --kubeconfig "$KUBECONFIG" --wait 180s
fi

[[ "$(kind get nodes --name "$CLUSTER_NAME")" == "$node" ]] ||
  die "requires exactly one control-plane node named $node; existing cluster unchanged"
kind export kubeconfig --name "$CLUSTER_NAME" --kubeconfig "$KUBECONFIG"
kubectl --kubeconfig "$KUBECONFIG" --context "$context" --request-timeout=10s \
  wait --for=condition=Ready "node/$node" --timeout=180s
node_json="$(kubectl --kubeconfig "$KUBECONFIG" --context "$context" --request-timeout=10s \
  get node "$node" -o json)"
node_arch="$(jq -er '.status.nodeInfo.architecture' <<<"$node_json")"
[[ "$node_arch" == "$arch" ]] || die "node architecture $node_arch does not match Docker $arch"
actual_family="$(jq -er '
  .spec.podCIDRs | map(if contains(":") then "ipv6" else "ipv4" end) | sort |
  if . == ["ipv4", "ipv6"] then "dual"
  elif . == ["ipv4"] then "ipv4"
  elif . == ["ipv6"] then "ipv6"
  else error("invalid pod CIDRs") end' <<<"$node_json")"
[[ "$actual_family" == "$IP_FAMILY" ]] ||
  die "existing cluster uses $actual_family, requested $IP_FAMILY; choose another name or explicitly delete the cluster"

mkdir -p -- "$(dirname -- "$KUBECONFIG_PATH")"
kind export kubeconfig --name "$CLUSTER_NAME" --kubeconfig "$KUBECONFIG_PATH"
printf '[setup-kind] Ready: %s (linux/%s, %s). Kubeconfig: %s\n' \
  "$context" "$arch" "$IP_FAMILY" "$KUBECONFIG_PATH"
