#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Prepare a fresh Ubuntu VM for the complete local NICo DevSpace stack.
#
# Docker and containerd data are placed under an existing /dockerroot directory,
# or a symlink to one, by default.

set -euo pipefail

DEVSPACE_VERSION="v6.3.21"
KIND_VERSION="v0.32.0"
KUBECTL_VERSION="v1.36.3"
HELM_VERSION="v3.21.3"
KIND_NODE_IMAGE="kindest/node:v1.36.1"

DEV_USER=""
REPO_DIR=""
REPO_URL="https://github.com/NVIDIA/infra-controller.git"
REPO_REF=""
CLUSTER_NAME="nico-dev"
DOCKER_ROOT="/dockerroot"
SKIP_DEPLOY=0
SCRIPT_START_SECONDS="${SECONDS}"

format_duration() {
  local total_seconds="$1"
  printf '%02d:%02d:%02d' \
    "$((total_seconds / 3600))" \
    "$(((total_seconds % 3600) / 60))" \
    "$((total_seconds % 60))"
}

log() {
  printf '[setup-devspace-on-host] %s\n' "$*"
}

die() {
  printf '[setup-devspace-on-host] error: %s\n' "$*" >&2
  exit 1
}

report_duration() {
  local status=$?
  trap - EXIT
  local outcome="completed"
  if [[ "${status}" -ne 0 ]]; then
    outcome="failed with status ${status}"
  fi
  log "Run ${outcome}; total duration: $(
    format_duration "$((SECONDS - SCRIPT_START_SECONDS))"
  )"
  exit "${status}"
}

usage() {
  cat <<'EOF'
Usage:
  setup-devspace-on-host.sh [options]

Options:
  --user USER          Developer account to configure. Defaults to the user
                       that invoked sudo.
  --repo-dir PATH      Existing checkout or clone destination. Defaults to
                       ~/infra-controller.
  --repo-url URL       Repository to clone if --repo-dir does not exist.
  --repo-ref REF       Branch, tag, or commit to check out after cloning.
  --cluster-name NAME  kind cluster name. Default: nico-dev.
  --docker-root PATH   Existing Docker storage directory, or symlink to one.
                       Default: /dockerroot.
  --skip-deploy        Prepare the host and cluster but do not build/deploy.
  -h, --help           Show this help.

Run this script as the intended developer or with --user:

  dev/deployment/devspace/setup-devspace-on-host.sh
  sudo dev/deployment/devspace/setup-devspace-on-host.sh --user devuser
EOF
}

while (($#)); do
  case "$1" in
    --user)
      (($# >= 2)) || die "--user requires a value"
      DEV_USER="$2"
      shift 2
      ;;
    --repo-dir)
      (($# >= 2)) || die "--repo-dir requires a value"
      REPO_DIR="$2"
      shift 2
      ;;
    --repo-url)
      (($# >= 2)) || die "--repo-url requires a value"
      REPO_URL="$2"
      shift 2
      ;;
    --repo-ref)
      (($# >= 2)) || die "--repo-ref requires a value"
      REPO_REF="$2"
      shift 2
      ;;
    --cluster-name)
      (($# >= 2)) || die "--cluster-name requires a value"
      CLUSTER_NAME="$2"
      shift 2
      ;;
    --docker-root)
      (($# >= 2)) || die "--docker-root requires a value"
      DOCKER_ROOT="$2"
      shift 2
      ;;
    --skip-deploy)
      SKIP_DEPLOY=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown option: $1"
      ;;
  esac
done

if [[ "${EUID}" -ne 0 ]]; then
  if [[ -z "${DEV_USER}" ]]; then
    DEV_USER="${USER:-$(id -un)}"
  fi
  sudo_args=(
    bash "$0"
    --user "${DEV_USER}" \
    --repo-url "${REPO_URL}" \
    --cluster-name "${CLUSTER_NAME}" \
    --docker-root "${DOCKER_ROOT}"
  )
  if [[ -n "${REPO_DIR}" ]]; then
    sudo_args+=(--repo-dir "${REPO_DIR}")
  fi
  if [[ -n "${REPO_REF}" ]]; then
    sudo_args+=(--repo-ref "${REPO_REF}")
  fi
  if [[ "${SKIP_DEPLOY}" == "1" ]]; then
    sudo_args+=(--skip-deploy)
  fi
  exec sudo "${sudo_args[@]}"
fi

trap report_duration EXIT

if [[ -z "${DEV_USER}" ]]; then
  DEV_USER="${SUDO_USER:-}"
fi
[[ -n "${DEV_USER}" ]] || die "use --user when running directly as root"
id "${DEV_USER}" >/dev/null 2>&1 || die "user does not exist: ${DEV_USER}"

USER_HOME="$(getent passwd "${DEV_USER}" | cut -d: -f6)"
USER_GROUP="$(id -gn "${DEV_USER}")"
[[ -n "${USER_HOME}" ]] || die "could not determine home for ${DEV_USER}"

case "$(uname -m)" in
  x86_64)
    DOWNLOAD_ARCH="amd64"
    ;;
  aarch64|arm64)
    DOWNLOAD_ARCH="arm64"
    ;;
  *)
    die "unsupported CPU architecture: $(uname -m)"
    ;;
esac

if [[ -z "${REPO_DIR}" ]]; then
  script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
  possible_repo="$(cd -- "${script_dir}/../../.." 2>/dev/null && pwd || true)"
  if [[ -f "${possible_repo}/devspace.yaml" ]]; then
    REPO_DIR="${possible_repo}"
  else
    REPO_DIR="${USER_HOME}/infra-controller"
  fi
fi

[[ -r /etc/os-release ]] || die "cannot determine the operating system"
# shellcheck source=/dev/null
source /etc/os-release
[[ "${ID:-}" == "ubuntu" ]] || die "this bootstrap currently supports Ubuntu only"
[[ -n "${VERSION_CODENAME:-}" ]] || \
  die "cannot determine the Ubuntu release codename"
[[ -n "${VERSION_ID:-}" ]] || die "cannot determine the Ubuntu release version"
dpkg --compare-versions "${VERSION_ID}" ge "22.04" || \
  die "unsupported Ubuntu release: ${VERSION_ID}; 22.04 or later is required"

run_as_user() {
  runuser -u "${DEV_USER}" -- env \
    HOME="${USER_HOME}" \
    USER="${DEV_USER}" \
    PATH="${USER_HOME}/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    GODEBUG="tlsmlkem=0" \
    "$@"
}

ensure_user_directory() {
  local directory="$1"
  if [[ -e "${directory}" && ! -d "${directory}" ]]; then
    die "expected a user directory at ${directory}"
  fi
  mkdir -p "${directory}"
  chown "${DEV_USER}:${USER_GROUP}" "${directory}"
}

prepare_user_tool_directories() {
  log "Preparing user-owned Helm directories"
  local directory
  for directory in \
    "${USER_HOME}/.config" \
    "${USER_HOME}/.config/helm" \
    "${USER_HOME}/.cache" \
    "${USER_HOME}/.cache/helm" \
    "${USER_HOME}/.local" \
    "${USER_HOME}/.local/share" \
    "${USER_HOME}/.local/share/helm"; do
    ensure_user_directory "${directory}"
  done
}

install_host_packages() {
  log "Installing host packages"
  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y \
    ca-certificates \
    curl \
    git \
    jq \
    openssh-client \
    rsync

  if command -v docker >/dev/null 2>&1 && \
    command -v containerd >/dev/null 2>&1 && \
    docker buildx version >/dev/null 2>&1; then
    return
  fi

  log "Installing Docker CE"
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/ubuntu/gpg \
    -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc

  cat >/etc/apt/sources.list.d/docker.sources <<EOF
Types: deb
URIs: https://download.docker.com/linux/ubuntu
Suites: ${VERSION_CODENAME}
Components: stable
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/docker.asc
EOF

  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y \
    containerd.io \
    docker-buildx-plugin \
    docker-ce \
    docker-ce-cli \
    docker-compose-plugin
}

require_docker_storage() {
  [[ -d "${DOCKER_ROOT}" ]] || \
    die "Docker storage directory or directory symlink does not exist: ${DOCKER_ROOT}"
}

configure_docker() {
  log "Configuring Docker and containerd storage under ${DOCKER_ROOT}"
  systemctl stop docker.service docker.socket containerd.service \
    >/dev/null 2>&1 || true

  mkdir -p /etc/docker /etc/containerd
  mkdir -p "${DOCKER_ROOT}/containerd"

  local daemon_tmp
  daemon_tmp="$(mktemp)"
  if [[ -s /etc/docker/daemon.json ]]; then
    jq --arg root "${DOCKER_ROOT}" '. + {"data-root": $root}' \
      /etc/docker/daemon.json >"${daemon_tmp}"
  else
    jq -n --arg root "${DOCKER_ROOT}" '{"data-root": $root}' >"${daemon_tmp}"
  fi
  install -m 0644 "${daemon_tmp}" /etc/docker/daemon.json
  rm -f "${daemon_tmp}"

  if [[ ! -s /etc/containerd/config.toml ]]; then
    containerd config default >/etc/containerd/config.toml
  fi
  local containerd_tmp
  containerd_tmp="$(mktemp)"
  if grep -Eq '^root[[:space:]]*=' \
    /etc/containerd/config.toml; then
    sed -E \
      "s|^root[[:space:]]*=.*$|root = \"${DOCKER_ROOT}/containerd\"|" \
      /etc/containerd/config.toml >"${containerd_tmp}"
  else
    {
      printf 'root = "%s/containerd"\n\n' "${DOCKER_ROOT}"
      cat /etc/containerd/config.toml
    } >"${containerd_tmp}"
  fi
  install -m 0644 "${containerd_tmp}" /etc/containerd/config.toml
  rm -f "${containerd_tmp}"

  install -m 0755 -d \
    /etc/systemd/system/docker.service.d \
    /etc/systemd/system/containerd.service.d
  cat >/etc/systemd/system/docker.service.d/10-tls-compat.conf <<'EOF'
[Service]
Environment="GODEBUG=tlsmlkem=0"
EOF
  cat >/etc/systemd/system/containerd.service.d/10-tls-compat.conf <<'EOF'
[Service]
Environment="GODEBUG=tlsmlkem=0"
EOF

  usermod -aG docker "${DEV_USER}"
  systemctl daemon-reload
  systemctl enable --now containerd.service docker.service

  local _attempt
  for _attempt in {1..30}; do
    if docker info >/dev/null 2>&1; then
      break
    fi
    sleep 1
  done
  docker info >/dev/null 2>&1 || die "Docker did not become ready"
  run_as_user docker info >/dev/null 2>&1 || \
    die "${DEV_USER} cannot access Docker after being added to the docker group"
}

verify_checksum_file() {
  local artifact="$1"
  local checksum_file="$2"
  local expected actual
  expected="$(awk '{print $1; exit}' "${checksum_file}")"
  actual="$(sha256sum "${artifact}" | awk '{print $1}')"
  [[ "${actual}" == "${expected}" ]] || die "checksum failed for ${artifact}"
}

install_cli_tools() (
  log "Installing pinned DevSpace, kind, kubectl, and Helm binaries"
  local install_dir="${USER_HOME}/.local/bin"
  local work_dir
  local devspace_binary="devspace-linux-${DOWNLOAD_ARCH}"
  local kind_binary="kind-linux-${DOWNLOAD_ARCH}"
  work_dir="$(mktemp -d)"
  trap 'rm -rf -- "${work_dir}"' EXIT
  install -o "${DEV_USER}" -g "${USER_GROUP}" -m 0755 -d "${install_dir}"

  pushd "${work_dir}" >/dev/null

  curl -fsSLO \
    "https://github.com/loft-sh/devspace/releases/download/${DEVSPACE_VERSION}/${devspace_binary}"
  curl -fsSLO \
    "https://github.com/loft-sh/devspace/releases/download/${DEVSPACE_VERSION}/${devspace_binary}.sha256"
  verify_checksum_file "${devspace_binary}" "${devspace_binary}.sha256"
  install -o "${DEV_USER}" -g "${USER_GROUP}" -m 0755 \
    "${devspace_binary}" "${install_dir}/devspace"

  curl -fsSLO \
    "https://github.com/kubernetes-sigs/kind/releases/download/${KIND_VERSION}/${kind_binary}"
  curl -fsSLO \
    "https://github.com/kubernetes-sigs/kind/releases/download/${KIND_VERSION}/${kind_binary}.sha256sum"
  verify_checksum_file "${kind_binary}" "${kind_binary}.sha256sum"
  install -o "${DEV_USER}" -g "${USER_GROUP}" -m 0755 \
    "${kind_binary}" "${install_dir}/kind"

  curl -fsSLO \
    "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${DOWNLOAD_ARCH}/kubectl"
  curl -fsSLO \
    "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${DOWNLOAD_ARCH}/kubectl.sha256"
  printf '%s  %s\n' "$(cat kubectl.sha256)" kubectl | sha256sum --check
  install -o "${DEV_USER}" -g "${USER_GROUP}" -m 0755 \
    kubectl "${install_dir}/kubectl"

  local helm_archive="helm-${HELM_VERSION}-linux-${DOWNLOAD_ARCH}.tar.gz"
  curl -fsSLO "https://get.helm.sh/${helm_archive}"
  curl -fsSLO "https://get.helm.sh/${helm_archive}.sha256sum"
  verify_checksum_file "${helm_archive}" "${helm_archive}.sha256sum"
  tar -xzf "${helm_archive}"
  install -o "${DEV_USER}" -g "${USER_GROUP}" -m 0755 \
    "linux-${DOWNLOAD_ARCH}/helm" "${install_dir}/helm"

  popd >/dev/null

  run_as_user devspace version
  run_as_user kind version
  run_as_user kubectl version --client
  run_as_user helm version --short
)

configure_user_shell() {
  local login_shell marker end_marker rc_file candidate
  login_shell="$(getent passwd "${DEV_USER}" | cut -d: -f7)"
  marker="# NICo DevSpace VM bootstrap"
  end_marker="# end NICo DevSpace VM bootstrap"

  case "${login_shell}" in
    *csh)
      rc_file="${USER_HOME}/.cshrc"
      if [[ ! -f "${rc_file}" ]] || ! grep -Fq "${marker}" "${rc_file}"; then
        {
          printf '\n%s\n' "${marker}"
          # shellcheck disable=SC2016 # Keep expansion in the user's shell.
          printf 'setenv PATH "$HOME/.local/bin:$PATH"\n'
          printf 'setenv GODEBUG tlsmlkem=0\n'
          printf '%s\n' "${end_marker}"
        } >>"${rc_file}"
      fi
      ;;
    *)
      rc_file="${USER_HOME}/.profile"
      for candidate in .bash_profile .bash_login .profile; do
        if [[ -f "${USER_HOME}/${candidate}" ]]; then
          rc_file="${USER_HOME}/${candidate}"
          break
        fi
      done
      if [[ ! -f "${rc_file}" ]] || ! grep -Fq "${marker}" "${rc_file}"; then
        {
          printf '\n%s\n' "${marker}"
          # shellcheck disable=SC2016 # Keep expansion in the user's shell.
          printf 'export PATH="$HOME/.local/bin:$PATH"\n'
          printf 'export GODEBUG=tlsmlkem=0\n'
          printf '%s\n' "${end_marker}"
        } >>"${rc_file}"
      fi
      ;;
  esac
  chown "${DEV_USER}:${USER_GROUP}" "${rc_file}"
}

prepare_checkout() {
  if [[ ! -f "${REPO_DIR}/devspace.yaml" ]]; then
    [[ ! -e "${REPO_DIR}" ]] || \
      die "${REPO_DIR} exists but is not an infra-controller checkout"
    log "Cloning ${REPO_URL} into ${REPO_DIR}"
    run_as_user git clone "${REPO_URL}" "${REPO_DIR}"
  fi

  if [[ -n "${REPO_REF}" ]]; then
    log "Checking out ${REPO_REF}"
    run_as_user git -C "${REPO_DIR}" fetch origin "${REPO_REF}"
    run_as_user git -C "${REPO_DIR}" checkout --detach FETCH_HEAD
  fi
}

configure_kind_node_tls() {
  local node="${CLUSTER_NAME}-control-plane"
  log "Applying the registry TLS compatibility setting inside ${node}"
  run_as_user docker exec "${node}" \
    mkdir -p /etc/systemd/system/containerd.service.d
  printf '%s\n' \
    '[Service]' \
    'Environment="GODEBUG=tlsmlkem=0"' |
    run_as_user docker exec -i "${node}" \
      tee /etc/systemd/system/containerd.service.d/10-tls-compat.conf \
      >/dev/null
  run_as_user docker exec "${node}" systemctl daemon-reload
  run_as_user docker exec "${node}" systemctl restart containerd
}

prepare_cluster() {
  local clusters
  clusters="$(run_as_user kind get clusters)"
  if grep -Fxq "${CLUSTER_NAME}" <<<"${clusters}"; then
    log "Reusing kind cluster ${CLUSTER_NAME}"
    run_as_user kind export kubeconfig --name "${CLUSTER_NAME}"
  else
    log "Creating kind cluster ${CLUSTER_NAME} with ${KIND_NODE_IMAGE}"
    run_as_user docker pull "${KIND_NODE_IMAGE}"
    run_as_user kind create cluster \
      --name "${CLUSTER_NAME}" \
      --image "${KIND_NODE_IMAGE}"
  fi

  configure_kind_node_tls
  run_as_user kubectl wait --for=condition=Ready "node/${CLUSTER_NAME}-control-plane" \
    --timeout=180s
}

kind_node_has_image() {
  local node="$1"
  local image="$2"
  local images

  images="$(run_as_user docker exec "${node}" ctr -n k8s.io images list -q)" || return
  grep -Fxq "${image}" <<<"${images}"
}

preload_postgres_image() {
  local node="${CLUSTER_NAME}-control-plane"
  local host_image="postgres:14.5-alpine"
  local node_image="docker.io/library/postgres:14.5-alpine"

  if kind_node_has_image "${node}" "${node_image}"; then
    log "Reusing ${node_image} inside ${node}"
    return
  fi

  if ! run_as_user docker image inspect "${host_image}" >/dev/null 2>&1; then
    log "Pulling ${host_image} through the host Docker client"
    local attempt
    for attempt in {1..6}; do
      if run_as_user docker pull "${host_image}"; then
        break
      fi
      if [[ "${attempt}" == "6" ]]; then
        die "could not pull ${host_image}"
      fi
      sleep $((attempt * 10))
    done
  fi

  log "Loading ${host_image} into ${node}"
  run_as_user docker save "${host_image}" |
    run_as_user docker exec -i "${node}" \
      ctr -n k8s.io images import --digests --snapshotter=overlayfs -

  kind_node_has_image "${node}" "${node_image}" ||
    die "${node_image} was not loaded into ${node}"
}

cache_postgres_wait_image() {
  local node="${CLUSTER_NAME}-control-plane"
  local source_image="docker.io/library/postgres:14.5-alpine"
  local wait_image="docker.io/library/postgres:14.4-alpine"

  if kind_node_has_image "${node}" "${wait_image}"; then
    return
  fi
  if ! kind_node_has_image "${node}" "${source_image}"; then
    die "bootstrap did not load the expected ${source_image} image"
  fi

  # REST migration manifests require 14.4; alias the preloaded 14.5 image so
  # local setup does not need to pull a second PostgreSQL image into kind.
  log "Caching ${source_image} as the REST migration wait image"
  run_as_user docker exec "${node}" ctr -n k8s.io images tag \
    "${source_image}" "${wait_image}"
}

deploy_stack() {
  log "Bootstrapping Kubernetes prerequisites"
  # shellcheck disable=SC2016 # $1 is intentionally expanded by the child shell.
  run_as_user bash -c 'cd "$1" && dev/deployment/devspace/bootstrap-prereqs.sh' \
    _ "${REPO_DIR}"
  cache_postgres_wait_image

  log "Building and deploying the complete stack"
  # shellcheck disable=SC2016 # $1 is intentionally expanded by the child shell.
  run_as_user bash -c 'cd "$1" && devspace deploy -n nico-system' \
    _ "${REPO_DIR}"
}

temporal_namespace_is_healthy() {
  run_as_user kubectl -n temporal exec deployment/temporal-admintools -- \
    temporal operator namespace describe \
      --namespace site \
      --address temporal-frontend.temporal.svc.cluster.local.:7233 \
      --tls-cert-path /var/secrets/temporal/certs/server-interservice/tls.crt \
      --tls-key-path /var/secrets/temporal/certs/server-interservice/tls.key \
      --tls-ca-path /var/secrets/temporal/certs/server-interservice/ca.crt \
      --tls-server-name interservice.server.temporal.local >/dev/null
}

temporal_restart_counts() {
  run_as_user kubectl -n temporal get pods \
    -l app.kubernetes.io/part-of=temporal \
    -o jsonpath='{range .items[*]}{.metadata.name}{"="}{range .status.containerStatuses[*]}{.restartCount}{","}{end}{"\n"}{end}' | \
    LC_ALL=C sort
}

verify_deployed_stack() {
  log "Verifying deployed stack health"

  local deployment
  for deployment in frontend history matching worker; do
    run_as_user kubectl rollout status \
      "deployment/temporal-${deployment}" -n temporal --timeout=120s >/dev/null || \
      die "Temporal ${deployment} deployment is not ready"
  done

  run_as_user kubectl -n postgres exec statefulset/postgres -- \
    psql -U postgres -d postgres -tAc 'SELECT 1' >/dev/null || \
    die "PostgreSQL is not responding"

  local restart_counts_before
  local restart_counts_after
  restart_counts_before="$(temporal_restart_counts)"

  local attempt
  for attempt in {1..3}; do
    temporal_namespace_is_healthy || \
      die "Temporal namespace query failed on health-check attempt ${attempt}"
    if [[ "${attempt}" != "3" ]]; then
      sleep 10
    fi
  done

  restart_counts_after="$(temporal_restart_counts)"
  if [[ "${restart_counts_before}" != "${restart_counts_after}" ]]; then
    printf '%s\n' "Temporal restart counts before health check:" \
      "${restart_counts_before}" \
      "Temporal restart counts after health check:" \
      "${restart_counts_after}" >&2
    die "a Temporal container restarted during the health check"
  fi

  log "Deployed stack health verified"
}

show_summary() {
  local node_ip
  node_ip="$(run_as_user docker inspect \
    --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' \
    "${CLUSTER_NAME}-control-plane")"
  log "Setup complete"
  run_as_user kubectl get deployments,statefulsets -A
  run_as_user docker system df
  df -h / "${DOCKER_ROOT}"
  cat <<EOF

Access the services from another machine with:

  ssh -N \\
    -L 18388:${node_ip}:30388 \\
    -L 18082:${node_ip}:30082 \\
    ${DEV_USER}@<vm-hostname>

REST health: http://localhost:18388/healthz
Keycloak:    http://localhost:18082/realms/nico-dev
EOF
}

main() {
  require_docker_storage
  install_host_packages
  configure_docker
  prepare_user_tool_directories
  install_cli_tools
  configure_user_shell
  prepare_checkout
  prepare_cluster
  preload_postgres_image

  if [[ "${SKIP_DEPLOY}" != "1" ]]; then
    deploy_stack
    verify_deployed_stack
  fi
  show_summary
}

main "$@"
