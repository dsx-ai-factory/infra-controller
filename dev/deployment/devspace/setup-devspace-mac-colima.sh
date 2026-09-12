#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Manage a Colima-backed DevSpace environment on Apple Silicon.
#
# Colima already runs on Apple's Virtualization.framework, so this replaces the
# VM lifecycle that a standalone Ubuntu VM would need. Its guest is Ubuntu 24.04 and
# would satisfy setup-devspace-on-host.sh's OS check, but that script installs
# Docker CE and rewrites the daemon's data-root and the containerd root, which
# would fight Colima's own Docker. Its host-preparation steps are reproduced
# here against the Colima Docker daemon instead.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../../.." && pwd)"

# Measured against a full Core build: the compile peaks near 8 GiB with the
# cluster running, so 16 GiB leaves headroom. Colima can grow its disk later
# but cannot shrink it.
COLIMA_PROFILE="${COLIMA_PROFILE:-default}"
COLIMA_CPUS="${COLIMA_CPUS:-6}"
COLIMA_MEMORY_GIB="${COLIMA_MEMORY_GIB:-16}"
COLIMA_DISK_GIB="${COLIMA_DISK_GIB:-200}"

CLUSTER_NAME="${CLUSTER_NAME:-nico-dev}"

# How long a container or build cache record has to have gone unused before
# `prune` reclaims it.
PRUNE_UNUSED_FOR="${PRUNE_UNUSED_FOR:-6h}"
# shellcheck source=versions.env
source "${SCRIPT_DIR}/versions.env"

# Kept out of the Homebrew prefix so the pinned Helm 3 does not shadow an
# existing Helm 4 for anything but this stack.
TOOL_DIR="${TOOL_DIR:-${HOME}/.nico-devspace/bin}"

# Go 1.24 offers X25519MLKEM768 by default and some registry endpoints abort the
# handshake instead of negotiating down. setup-devspace-on-host.sh writes this
# into the guest shell and both systemd units for the same reason.
export GODEBUG="${GODEBUG:-tlsmlkem=0}"
export PATH="${TOOL_DIR}:${PATH}"

ACTION=up
ACTION_ARGS=()
RUN_START_EPOCH=0
TIMING_NAMES=()
TIMING_SECONDS=()

log() {
  printf '[setup-devspace-mac-colima] %s\n' "$*"
}

die() {
  printf '[setup-devspace-mac-colima] error: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage:
  dev/deployment/devspace/setup-devspace-mac-colima.sh [action]

Actions:
  up          Start Colima, install pinned tooling, create kind, verify.
  recreate    Delete and re-create the Colima profile, then run up.
  grow-disk   Grow the Lima disk to COLIMA_DISK_GIB, keeping all Docker data.
  tooling     Install the pinned DevSpace, kind, kubectl, and Helm binaries.
  cluster     Create the kind cluster and preload its images.
  verify      Verify Colima, pinned tooling, images, and the kind cluster.
  deploy      Bootstrap prerequisites, run devspace deploy, then prune.
  prune       Drop superseded image tags, stopped containers, and build cache
              unused for PRUNE_UNUSED_FOR. Keeps the Cargo cache mounts.
  status      Show the Colima, Docker, and cluster state.
  reset       Delete the kind cluster. Leaves Colima and its images alone.
  help        Show this help.
EOF
  # Expanding heredoc so the reported values are the ones this run will use,
  # including any overrides, rather than a copy that drifts from versions.env.
  cat <<EOF

Resources:
  Profile:      ${COLIMA_PROFILE}
  CPUs/memory:  ${COLIMA_CPUS} CPUs / ${COLIMA_MEMORY_GIB} GiB
  Disk:         ${COLIMA_DISK_GIB} GiB
  Hypervisor:   Apple Virtualization.framework via Colima's vz VM type
  Cluster:      kind ${CLUSTER_NAME} on ${KIND_NODE_IMAGE}
  Tooling:      DevSpace ${DEVSPACE_VERSION}, kind ${KIND_VERSION},
                kubectl ${KUBECTL_VERSION}, Helm ${HELM_VERSION}
  Tool dir:     ${TOOL_DIR}
  Prune age:    ${PRUNE_UNUSED_FOR}
EOF
  cat <<'EOF'

The checkout stays on the macOS filesystem and reaches the Docker daemon over
virtiofs, so every build context is read across that share.

Override any of the above with COLIMA_PROFILE, COLIMA_CPUS, COLIMA_MEMORY_GIB,
COLIMA_DISK_GIB, CLUSTER_NAME, TOOL_DIR, or PRUNE_UNUSED_FOR. Toolchain and
image pins live in versions.env, shared with the other setup scripts.
EOF
}

format_duration() {
  local elapsed="$1"
  printf '%02d:%02d:%02d' \
    "$((elapsed / 3600))" "$(((elapsed % 3600) / 60))" "$((elapsed % 60))"
}

run_timed() {
  local label="$1" start elapsed index
  shift
  start="$(date +%s)"
  log "Starting: ${label}"
  "$@"
  elapsed=$(( $(date +%s) - start ))
  index="${#TIMING_NAMES[@]}"
  TIMING_NAMES[index]="${label}"
  TIMING_SECONDS[index]="${elapsed}"
  log "Finished: ${label} ($(format_duration "${elapsed}"))"
}

report_timings() {
  local index total
  total=$(( $(date +%s) - RUN_START_EPOCH ))
  printf '\nTiming summary:\n'
  for index in "${!TIMING_NAMES[@]}"; do
    printf '  %-35s %s\n' \
      "${TIMING_NAMES[index]}" "$(format_duration "${TIMING_SECONDS[index]}")"
  done
  printf '  %-35s %s\n' Total "$(format_duration "${total}")"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

# Homebrew installs CLI plugins under its own prefix, which the Docker CLI does
# not scan. Without the link every DevSpace image build fails on `unknown flag:
# --tag`, and only after the deploy has already started.
require_docker_buildx() {
  docker buildx version >/dev/null 2>&1 && return
  die "docker buildx is unavailable; link the Homebrew plugin with:
  mkdir -p ~/.docker/cli-plugins
  ln -sfn /opt/homebrew/opt/docker-buildx/bin/docker-buildx ~/.docker/cli-plugins/docker-buildx"
}

check_host() {
  [[ "$(uname -s)" == Darwin ]] || die "this script requires macOS"
  [[ "$(uname -m)" == arm64 ]] || die "this script requires Apple Silicon"
  [[ -f "${REPO_ROOT}/devspace.yaml" ]] || \
    die "could not find devspace.yaml under ${REPO_ROOT}"

  require_command colima
  require_command curl
  require_command docker
  require_command jq
  require_command shasum
  require_command tar
  require_docker_buildx

  mkdir -p -- "${TOOL_DIR}"
}

colima_profile_exists() {
  colima list --json 2>/dev/null |
    jq -e --arg name "${COLIMA_PROFILE}" 'select(.name == $name)' >/dev/null
}

# Colima names both its Lima instance and its persistent disk `colima` for the
# default profile and `colima-<profile>` for any other, so neither one is
# addressable by the profile name.
lima_name() {
  if [[ "${COLIMA_PROFILE}" == default ]]; then
    printf 'colima\n'
  else
    printf 'colima-%s\n' "${COLIMA_PROFILE}"
  fi
}

limactl_disk() {
  LIMA_HOME="${HOME}/.colima/_lima" limactl disk "$@"
}

lima_disk_gib() {
  limactl_disk list 2>/dev/null |
    awk -v name="$(lima_name)" '$1 == name {sub(/GiB$/, "", $2); print $2; exit}'
}

# Colima reports `Broken` and `not running` when its own state file disagrees
# with a VM that is in fact serving Docker. A reachable daemon is the condition
# that matters here, so it wins over the status output.
colima_is_running() {
  colima status --profile "${COLIMA_PROFILE}" >/dev/null 2>&1 && return 0
  docker info >/dev/null 2>&1
}

# Colima can grow a disk but cannot shrink one, and CPU or memory changes only
# apply through a stop/start. Report a mismatch rather than silently running on
# a profile that does not match these values.
check_colima_resources() {
  local config="${HOME}/.colima/${COLIMA_PROFILE}/colima.yaml" actual
  [[ -f "${config}" ]] || return 0

  local -a problems=()
  actual="$(awk '/^cpu:/ {print $2; exit}' "${config}")"
  [[ "${actual}" == "${COLIMA_CPUS}" ]] || \
    problems+=("cpu is ${actual}, expected ${COLIMA_CPUS}")
  actual="$(awk '/^memory:/ {print $2; exit}' "${config}")"
  [[ "${actual}" == "${COLIMA_MEMORY_GIB}" ]] || \
    problems+=("memory is ${actual} GiB, expected ${COLIMA_MEMORY_GIB} GiB")
  # colima.yaml records the requested disk size even when Colima reused a
  # pre-existing Lima disk at its old size, so ask Lima for the real one.
  actual="$(lima_disk_gib)"
  [[ -z "${actual}" || "${actual}" == "${COLIMA_DISK_GIB}" ]] || \
    problems+=("disk is ${actual} GiB, expected ${COLIMA_DISK_GIB} GiB (use 'grow-disk')")
  actual="$(awk '/^vmType:/ {print $2; exit}' "${config}")"
  [[ "${actual}" == vz ]] || \
    problems+=("vmType is ${actual}, expected vz")

  ((${#problems[@]} == 0)) && return 0
  printf '[setup-devspace-mac-colima] profile %s does not match:\n' \
    "${COLIMA_PROFILE}" >&2
  printf '  %s\n' "${problems[@]}" >&2
  die "run 'grow-disk' for a disk mismatch, or 'recreate' to rebuild the profile"
}

# Growing the disk keeps every image, volume, and build cache entry, so prefer
# it over recreate whenever only the size is wrong. Lima refuses to resize a
# disk that an instance still holds, so the VM has to stop first.
grow_disk() {
  local actual
  actual="$(lima_disk_gib)"
  [[ -n "${actual}" ]] || die "no Lima disk named $(lima_name) exists yet"
  if [[ "${actual}" == "${COLIMA_DISK_GIB}" ]]; then
    log "Lima disk $(lima_name) is already ${COLIMA_DISK_GIB} GiB"
    return
  fi
  ((actual < COLIMA_DISK_GIB)) || \
    die "Lima cannot shrink ${actual} GiB to ${COLIMA_DISK_GIB} GiB; use 'recreate'"

  log "Stopping Colima so Lima can release the disk"
  colima stop --profile "${COLIMA_PROFILE}"
  log "Resizing $(lima_name) from ${actual} GiB to ${COLIMA_DISK_GIB} GiB"
  limactl_disk resize "$(lima_name)" --size "${COLIMA_DISK_GIB}GiB"
  log "Starting Colima"
  colima start --profile "${COLIMA_PROFILE}"
  limactl_disk list
}

start_colima() {
  if colima_is_running; then
    log "Reusing running Colima profile ${COLIMA_PROFILE}"
    check_colima_resources
    return
  fi

  if colima_profile_exists; then
    check_colima_resources
    log "Starting existing Colima profile ${COLIMA_PROFILE}"
    colima start --profile "${COLIMA_PROFILE}"
    return
  fi

  log "Creating Colima profile ${COLIMA_PROFILE} with ${COLIMA_CPUS} CPUs, ${COLIMA_MEMORY_GIB} GiB, ${COLIMA_DISK_GIB} GiB"
  colima start --profile "${COLIMA_PROFILE}" \
    --arch aarch64 \
    --cpu "${COLIMA_CPUS}" \
    --memory "${COLIMA_MEMORY_GIB}" \
    --disk "${COLIMA_DISK_GIB}" \
    --vm-type vz \
    --mount-type virtiofs
}

# `colima delete` removes the VM but leaves Lima's named persistent disk, which
# is what backs /mnt and therefore /var/lib/docker. A COLIMA_DISK_GIB change is
# silently ignored until that disk is deleted too, so do it here rather than
# writing a size into colima.yaml that the guest never sees.
recreate_colima() {
  if colima_profile_exists; then
    log "Deleting Colima profile ${COLIMA_PROFILE} and its images"
    colima delete --profile "${COLIMA_PROFILE}" --force
  fi
  if [[ -n "$(lima_disk_gib)" ]]; then
    log "Deleting the Lima disk $(lima_name) so ${COLIMA_DISK_GIB} GiB takes effect"
    limactl_disk delete "$(lima_name)"
  fi
  start_colima
}

verify_checksum() {
  local artifact="$1" expected="$2" actual
  actual="$(shasum -a 256 "${artifact}" | awk '{print $1}')"
  [[ "${actual}" == "${expected}" ]] || die "checksum failed for ${artifact}"
}

# The darwin/arm64 counterpart of install_cli_tools in
# setup-devspace-on-host.sh, pinned to the same versions its verify asserts.
install_tooling() {
  local work_dir expected
  work_dir="$(mktemp -d)"
  # shellcheck disable=SC2064 # Expand work_dir now, not when the trap fires.
  trap "rm -rf -- '${work_dir}'" RETURN

  log "Installing pinned DevSpace, kind, kubectl, and Helm into ${TOOL_DIR}"
  (
    cd -- "${work_dir}"

    curl -fsSLO "https://github.com/loft-sh/devspace/releases/download/${DEVSPACE_VERSION}/devspace-darwin-arm64"
    curl -fsSLO "https://github.com/loft-sh/devspace/releases/download/${DEVSPACE_VERSION}/devspace-darwin-arm64.sha256"
    expected="$(awk '{print $1; exit}' devspace-darwin-arm64.sha256)"
    verify_checksum devspace-darwin-arm64 "${expected}"
    install -m 0755 devspace-darwin-arm64 "${TOOL_DIR}/devspace"

    curl -fsSLO "https://github.com/kubernetes-sigs/kind/releases/download/${KIND_VERSION}/kind-darwin-arm64"
    curl -fsSLO "https://github.com/kubernetes-sigs/kind/releases/download/${KIND_VERSION}/kind-darwin-arm64.sha256sum"
    expected="$(awk '{print $1; exit}' kind-darwin-arm64.sha256sum)"
    verify_checksum kind-darwin-arm64 "${expected}"
    install -m 0755 kind-darwin-arm64 "${TOOL_DIR}/kind"

    curl -fsSLO "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/darwin/arm64/kubectl"
    curl -fsSLO "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/darwin/arm64/kubectl.sha256"
    verify_checksum kubectl "$(cat kubectl.sha256)"
    install -m 0755 kubectl "${TOOL_DIR}/kubectl"

    local helm_archive="helm-${HELM_VERSION}-darwin-arm64.tar.gz"
    curl -fsSLO "https://get.helm.sh/${helm_archive}"
    curl -fsSLO "https://get.helm.sh/${helm_archive}.sha256sum"
    expected="$(awk '{print $1; exit}' "${helm_archive}.sha256sum")"
    verify_checksum "${helm_archive}" "${expected}"
    tar -xzf "${helm_archive}"
    install -m 0755 darwin-arm64/helm "${TOOL_DIR}/helm"
  )
}

kind_node_has_image() {
  local node="$1" image="$2"
  docker exec "${node}" ctr -n k8s.io images list -q 2>/dev/null |
    grep -Fxq "${image}"
}

# Mirrors configure_kind_node_tls: the node pulls images through its own
# containerd, which needs the same handshake workaround as the host.
configure_kind_node_tls() {
  local node="${CLUSTER_NAME}-control-plane"
  log "Applying the registry TLS compatibility setting inside ${node}"
  docker exec "${node}" mkdir -p /etc/systemd/system/containerd.service.d
  printf '%s\n' '[Service]' 'Environment="GODEBUG=tlsmlkem=0"' |
    docker exec -i "${node}" \
      tee /etc/systemd/system/containerd.service.d/10-tls-compat.conf >/dev/null
  docker exec "${node}" systemctl daemon-reload
  docker exec "${node}" systemctl restart containerd
}

# Mirrors preload_postgres_image and cache_postgres_wait_image. The REST
# migration manifests wait on 14.4, so the 14.5 image is aliased rather than
# pulled a second time.
preload_postgres_image() {
  local node="${CLUSTER_NAME}-control-plane"
  local node_image="docker.io/library/${CORE_POSTGRES_IMAGE}"
  local wait_image="docker.io/library/${REST_POSTGRES_IMAGE}"

  if ! kind_node_has_image "${node}" "${node_image}"; then
    docker image inspect "${CORE_POSTGRES_IMAGE}" >/dev/null 2>&1 || \
      docker pull "${CORE_POSTGRES_IMAGE}"
    log "Loading ${CORE_POSTGRES_IMAGE} into ${node}"
    docker save "${CORE_POSTGRES_IMAGE}" |
      docker exec -i "${node}" \
        ctr -n k8s.io images import --digests --snapshotter=overlayfs -
  fi

  kind_node_has_image "${node}" "${wait_image}" && return
  log "Caching ${node_image} as the REST migration wait image"
  docker exec "${node}" ctr -n k8s.io images tag "${node_image}" "${wait_image}"
}

ensure_cluster() {
  if kind get clusters 2>/dev/null | grep -Fxq "${CLUSTER_NAME}"; then
    log "Reusing kind cluster ${CLUSTER_NAME}"
    kind export kubeconfig --name "${CLUSTER_NAME}"
  else
    log "Creating kind cluster ${CLUSTER_NAME} with ${KIND_NODE_IMAGE}"
    docker pull "${KIND_NODE_IMAGE}"
    kind create cluster --name "${CLUSTER_NAME}" --image "${KIND_NODE_IMAGE}"
  fi

  configure_kind_node_tls
  kubectl wait --for=condition=Ready "node/${CLUSTER_NAME}-control-plane" \
    --timeout=180s
  preload_postgres_image
}

# Colima keeps Docker data on its own single disk, so there is no /dockerroot
# bind mount to assert on here.
verify_environment() {
  log "Verifying Colima, pinned tooling, images, and the kind cluster"

  [[ "$(docker info --format '{{.Architecture}}')" == aarch64 ]] || \
    die "the Docker daemon is not aarch64"

  devspace version | grep -Fq "${DEVSPACE_VERSION#v}" || \
    die "devspace is not ${DEVSPACE_VERSION}"
  kind version | grep -Fq "${KIND_VERSION}" || die "kind is not ${KIND_VERSION}"
  kubectl version --client | grep -Fq "${KUBECTL_VERSION}" || \
    die "kubectl is not ${KUBECTL_VERSION}"
  helm version --short | grep -Fq "${HELM_VERSION}" || \
    die "helm is not ${HELM_VERSION}"

  [[ "$(kubectl config current-context)" == "kind-${CLUSTER_NAME}" ]] || \
    die "the current context is not kind-${CLUSTER_NAME}"
  kubectl wait --for=condition=Ready "node/${CLUSTER_NAME}-control-plane" \
    --timeout=120s
  [[ "$(kubectl get "node/${CLUSTER_NAME}-control-plane" \
    -o jsonpath='{.status.nodeInfo.architecture}')" == arm64 ]] || \
    die "the kind node is not arm64"
  [[ "$(docker image inspect "${KIND_NODE_IMAGE}" \
    --format '{{.Architecture}}')" == arm64 ]] || \
    die "${KIND_NODE_IMAGE} is not arm64"
  [[ "$(docker image inspect "${CORE_POSTGRES_IMAGE}" \
    --format '{{.Architecture}}')" == arm64 ]] || \
    die "${CORE_POSTGRES_IMAGE} is not arm64"

  colima status --profile "${COLIMA_PROFILE}"
  docker info --format 'Docker: {{.ServerVersion}}, {{.Architecture}}, root={{.DockerRootDir}}'
  kubectl get "node/${CLUSTER_NAME}-control-plane" \
    -o custom-columns=NAME:.metadata.name,ARCH:.status.nodeInfo.architecture,READY:.status.conditions[-1].status \
    --no-headers
}

# DevSpace gives every build a unique tag and never removes the previous one,
# so each deploy leaves a complete superseded image set on disk. One deploy cost
# 60 GiB without this.
DEVSPACE_IMAGE_REPOS=(
  nico-api
  nico-bmc-proxy
  machine-a-tron
  localhost:5000/nico-rest-api
  localhost:5000/nico-rest-workflow
  localhost:5000/nico-rest-site-manager
  localhost:5000/nico-rest-site-agent
  localhost:5000/nico-rest-db
  localhost:5000/nico-rest-cert-manager
  localhost:5000/nico-mcp
)

# Only removes build outputs. The build cache is deliberately left alone: its
# /cargo-target mount is what keeps the Rust build incremental, and dropping it
# costs a cold rebuild at a higher memory peak.
prune_images() {
  local repo image
  log "Removing superseded DevSpace image tags"
  for repo in "${DEVSPACE_IMAGE_REPOS[@]}"; do
    # `docker images` lists newest first, so every line after the first belongs
    # to an earlier build.
    while read -r image; do
      [[ -n "${image}" && "${image}" != *"<none>"* ]] || continue
      docker rmi "${image}" >/dev/null 2>&1 || true
    done < <(docker images "${repo}" --format '{{.Repository}}:{{.Tag}}' 2>/dev/null |
      tail -n +2)
  done
  log "Removing dangling images"
  docker image prune --force >/dev/null 2>&1 || true

  log "Removing containers stopped for ${PRUNE_UNUSED_FOR}"
  docker container prune --force --filter "until=${PRUNE_UNUSED_FOR}" \
    >/dev/null 2>&1 || true

  # Build cache goes last, because dropping the superseded images above is what
  # makes their layer records collectable. Every heavy step leaves one record
  # per build, so three deploys leave three 6 GB copies of the artifacts COPY.
  #
  # Filter by age, never by a size cap: `--keep-storage` cannot tell the
  # duplicated per-build records from the single /cargo-target mount that keeps
  # the Rust build incremental, and evicting that costs a cold rebuild. The
  # filter measures time since last use, and this runs directly after a deploy
  # when that mount was just written, so it is never a candidate here. Raise
  # PRUNE_UNUSED_FOR when running `prune` standalone long after a deploy.
  log "Removing build cache unused for ${PRUNE_UNUSED_FOR}"
  docker builder prune --force --filter "until=${PRUNE_UNUSED_FOR}" 2>&1 |
    tail -1

  docker system df
}

deploy_stack() {
  log "Bootstrapping Kubernetes prerequisites"
  (cd -- "${REPO_ROOT}" && dev/deployment/devspace/bootstrap-prereqs.sh)
  log "Building and deploying the complete stack"
  (cd -- "${REPO_ROOT}" && devspace deploy -n nico-system)
  prune_images
}

show_status() {
  colima status --profile "${COLIMA_PROFILE}" 2>&1 || true
  docker context show 2>/dev/null || true
  kind get clusters 2>/dev/null || true
  kubectl config current-context 2>/dev/null || true
}

reset_cluster() {
  if kind get clusters 2>/dev/null | grep -Fxq "${CLUSTER_NAME}"; then
    log "Deleting kind cluster ${CLUSTER_NAME}"
    kind delete cluster --name "${CLUSTER_NAME}"
  else
    log "kind cluster ${CLUSTER_NAME} does not exist"
  fi
}

show_summary() {
  cat <<EOF

Colima profile ${COLIMA_PROFILE} and kind cluster ${CLUSTER_NAME} are ready.

Put the pinned tooling ahead of Homebrew for this stack:
  export PATH="${TOOL_DIR}:\$PATH"
  export GODEBUG=tlsmlkem=0

Then deploy:
  dev/deployment/devspace/setup-devspace-mac-colima.sh deploy

EOF
}

parse_args() {
  (($# == 0)) && return
  ACTION="$1"
  shift
  ACTION_ARGS=("$@")
}

main() {
  parse_args "$@"
  case "${ACTION}" in
    help|-h|--help)
      usage
      ;;
    up)
      RUN_START_EPOCH="$(date +%s)"
      run_timed "Host validation" check_host
      run_timed "Colima start" start_colima
      run_timed "Pinned tooling install" install_tooling
      run_timed "kind cluster" ensure_cluster
      run_timed "Verification" verify_environment
      show_summary
      report_timings
      ;;
    recreate)
      RUN_START_EPOCH="$(date +%s)"
      run_timed "Host validation" check_host
      run_timed "Colima re-creation" recreate_colima
      run_timed "Pinned tooling install" install_tooling
      run_timed "kind cluster" ensure_cluster
      run_timed "Verification" verify_environment
      show_summary
      report_timings
      ;;
    grow-disk)
      check_host
      grow_disk
      ;;
    tooling)
      check_host
      install_tooling
      ;;
    cluster)
      check_host
      ensure_cluster
      ;;
    verify)
      check_host
      verify_environment
      ;;
    deploy)
      check_host
      deploy_stack
      ;;
    prune)
      check_host
      prune_images
      ;;
    status)
      show_status
      ;;
    reset)
      check_host
      reset_cluster
      ;;
    *)
      die "unknown action: ${ACTION} (run 'help' for usage)"
      ;;
  esac
}

main "$@"
