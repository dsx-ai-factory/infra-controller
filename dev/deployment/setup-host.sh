#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: setup-host.sh [--user USER]
Prepare Ubuntu 22.04+ ARM64/AMD64 for local native development.
Install host packages, Docker when absent, and pinned kind/kubectl/Helm/Tilt
in USER's ~/.local/bin. Existing complete, running Docker is preserved.
No cluster, stack, guest, shell profile, huge-page or watcher settings are changed.
--user defaults to the invoking user (SUDO_USER when root); root is not accepted.
Run from an existing checkout. Package installation requires sudo.
EOF
}
log() { printf '[setup-host] %s\n' "$*"; }
die() { printf '[setup-host] %s\n' "$*" >&2; exit 1; }
DEV_USER="${USER:-}"
if [[ $EUID -eq 0 ]]; then DEV_USER="${SUDO_USER:-}"; fi
while (($#)); do
  case "$1" in
    --user) [[ $# -ge 2 && -n "$2" && "$2" != -* ]] || die '--user requires a value'; DEV_USER="$2"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ -n "${DEV_USER}" && "${DEV_USER}" != -* ]] || die 'select a developer with --user'
case "$(uname -m)" in x86_64|aarch64|arm64) ;; *) die 'native ARM64 or AMD64 is required' ;; esac
[[ "$(uname -s)" == Linux && -r /etc/os-release ]] || die 'Ubuntu is required'
# shellcheck source=/dev/null
source /etc/os-release
[[ "${ID:-}" == ubuntu && -n "${VERSION_CODENAME:-}" && -n "${VERSION_ID:-}" ]] || die 'Ubuntu is required'
dpkg --compare-versions "${VERSION_ID}" ge 22.04 || die 'Ubuntu 22.04 or newer is required'
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
for helper in docker-host.sh install-tools.py; do
  [[ -r "${SCRIPT_DIR}/${helper}" ]] || die "checkout is missing ${helper}"
done
if [[ $EUID -ne 0 ]]; then exec sudo bash "$0" --user "${DEV_USER}"; fi
developer_uid="$(id -u "${DEV_USER}" 2>/dev/null)" || die "user does not exist: ${DEV_USER}"
[[ "${developer_uid}" != 0 ]] || die '--user must be a non-root account'
USER_HOME="$(getent passwd "${DEV_USER}" | cut -d: -f6)"
[[ -d "${USER_HOME}" ]] || die 'developer home directory is missing'
run_as_user() {
  runuser -u "${DEV_USER}" -- env HOME="${USER_HOME}" USER="${DEV_USER}" \
    DOCKER_HOST=unix:///var/run/docker.sock \
    PATH="${USER_HOME}/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" "$@"
}
export DOCKER_HOST=unix:///var/run/docker.sock
unset DOCKER_CONTEXT

prepare_host() {
# A partially installed runtime is an operator repair, not permission to replace it.
existing=0
if command -v docker >/dev/null 2>&1 || command -v containerd >/dev/null 2>&1; then
  command -v docker >/dev/null && command -v containerd >/dev/null && \
    docker buildx version >/dev/null && docker --host unix:///var/run/docker.sock info >/dev/null || \
    die 'existing Docker must include containerd/buildx and be running; repair it before bootstrap'
  getent group docker >/dev/null || die 'existing Docker must provide its docker access group'
  existing=1
fi
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-upgrade ca-certificates curl git jq make openssh-client python3 rsync
# shellcheck source=docker-host.sh
source "${SCRIPT_DIR}/docker-host.sh"
if [[ ${existing} == 0 ]]; then
  install_docker
  configure_docker
else
  usermod -aG docker "${DEV_USER}"
fi
run_as_user docker info >/dev/null
run_as_user python3 "${SCRIPT_DIR}/install-tools.py"
log 'Host ready. Start a new login session for docker group access and add ~/.local/bin to PATH.'
}

prepare_host
