#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Remove the state created by setup-devspace-on-host.sh.
#
# This script is intentionally dry-run by default. It preserves the VM-provided
# Docker packages and the Docker data directory itself, while removing the
# local kind cluster, Docker build data, checkout, tools, caches, shell setup,
# and host configuration written by the setup script. /dockerroot is preferred
# when present; otherwise the regular Docker data path is used.

set -euo pipefail

DEV_USER=""
REPO_DIR=""
CLUSTER_NAME="nico-dev"
DOCKER_ROOT="/dockerroot"
DOCKER_ROOT_EXPLICIT=0
CONFIRM_HOST=""
APPLY=0
FORCE_DIRTY_CHECKOUT=0
KEEP_CHECKOUT=0
KEEP_HOST_CONFIG=0
DOCKER_DATA_ROOT_IS_SETUP=0
CONTAINERD_ROOT_IS_SETUP=0
SCRIPT_START_SECONDS="${SECONDS}"

format_duration() {
  local total_seconds="$1"
  printf '%02d:%02d:%02d' \
    "$((total_seconds / 3600))" \
    "$(((total_seconds % 3600) / 60))" \
    "$((total_seconds % 60))"
}

log() {
  printf '[reset-devspace-on-host] %s\n' "$*"
}

die() {
  printf '[reset-devspace-on-host] error: %s\n' "$*" >&2
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
  reset-devspace-on-host.sh [options]

The default is a dry run. Destructive execution requires both --apply and
--confirm-host matching the VM's hostname.

Options:
  --user USER          Developer account to reset. Defaults to the user that
                       invoked sudo.
  --repo-dir PATH      Checkout to delete. Auto-detects ~/infra-controller or
                       the legacy ~/ncx-infra-controller-core path.
  --cluster-name NAME  kind cluster to delete. Default: nico-dev.
  --docker-root PATH   Docker data path. Defaults to /dockerroot when present,
                       otherwise Docker's active root or /var/lib/docker.
  --confirm-host NAME  Required with --apply; must match hostname or hostname -f.
  --apply              Perform the reset. Without this flag, print the plan.
  --force-dirty-checkout
                       Allow deletion when the checkout has uncommitted files.
  --keep-checkout      Preserve the repository checkout.
  --keep-host-config   Preserve Docker storage/TLS config and docker membership.
  -h, --help           Show this help.

Copy this script outside the checkout and first inspect the plan:

  bash /tmp/reset-devspace-on-host.sh \
    --user devuser \
    --repo-dir /home/devuser/infra-controller

Then apply it:

  bash /tmp/reset-devspace-on-host.sh \
    --apply \
    --confirm-host host.example.com \
    --user devuser \
    --repo-dir /home/devuser/infra-controller \
    --force-dirty-checkout
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
    --cluster-name)
      (($# >= 2)) || die "--cluster-name requires a value"
      CLUSTER_NAME="$2"
      shift 2
      ;;
    --docker-root)
      (($# >= 2)) || die "--docker-root requires a value"
      DOCKER_ROOT="$2"
      DOCKER_ROOT_EXPLICIT=1
      shift 2
      ;;
    --confirm-host)
      (($# >= 2)) || die "--confirm-host requires a value"
      CONFIRM_HOST="$2"
      shift 2
      ;;
    --apply)
      APPLY=1
      shift
      ;;
    --force-dirty-checkout)
      FORCE_DIRTY_CHECKOUT=1
      shift
      ;;
    --keep-checkout)
      KEEP_CHECKOUT=1
      shift
      ;;
    --keep-host-config)
      KEEP_HOST_CONFIG=1
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

if [[ "${EUID}" -ne 0 && "${APPLY}" == "1" ]]; then
  if [[ -z "${DEV_USER}" ]]; then
    DEV_USER="${USER}"
  fi
  sudo_args=(
    bash "$0"
    --user "${DEV_USER}"
    --cluster-name "${CLUSTER_NAME}"
  )
  if [[ "${DOCKER_ROOT_EXPLICIT}" == "1" ]]; then
    sudo_args+=(--docker-root "${DOCKER_ROOT}")
  fi
  if [[ -n "${REPO_DIR}" ]]; then
    sudo_args+=(--repo-dir "${REPO_DIR}")
  fi
  if [[ -n "${CONFIRM_HOST}" ]]; then
    sudo_args+=(--confirm-host "${CONFIRM_HOST}")
  fi
  if [[ "${APPLY}" == "1" ]]; then
    sudo_args+=(--apply)
  fi
  if [[ "${FORCE_DIRTY_CHECKOUT}" == "1" ]]; then
    sudo_args+=(--force-dirty-checkout)
  fi
  if [[ "${KEEP_CHECKOUT}" == "1" ]]; then
    sudo_args+=(--keep-checkout)
  fi
  if [[ "${KEEP_HOST_CONFIG}" == "1" ]]; then
    sudo_args+=(--keep-host-config)
  fi
  exec sudo "${sudo_args[@]}"
fi

trap report_duration EXIT

if [[ -z "${DEV_USER}" ]]; then
  DEV_USER="${SUDO_USER:-${USER:-}}"
fi
[[ -n "${DEV_USER}" ]] || die "use --user when running directly as root"
id "${DEV_USER}" >/dev/null 2>&1 || die "user does not exist: ${DEV_USER}"

USER_HOME="$(getent passwd "${DEV_USER}" | cut -d: -f6)"
USER_GROUP="$(id -gn "${DEV_USER}")"
[[ -n "${USER_HOME}" && "${USER_HOME}" != "/" ]] || \
  die "invalid home directory for ${DEV_USER}"

run_as_user() {
  local user_env=(
    HOME="${USER_HOME}"
    USER="${DEV_USER}"
    PATH="${USER_HOME}/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
    GODEBUG="tlsmlkem=0"
  )
  if [[ "${EUID}" -eq 0 ]]; then
    runuser -u "${DEV_USER}" -- env "${user_env[@]}" "$@"
  elif [[ "$(id -un)" == "${DEV_USER}" ]]; then
    env "${user_env[@]}" "$@"
  else
    die "dry run as another user requires sudo"
  fi
}

resolve_docker_root() {
  if [[ "${DOCKER_ROOT_EXPLICIT}" == "1" || -d "${DOCKER_ROOT}" ]]; then
    return
  fi

  local active_root
  active_root="$(docker info --format '{{.DockerRootDir}}' 2>/dev/null || true)"
  if [[ -n "${active_root}" ]]; then
    DOCKER_ROOT="${active_root}"
  else
    DOCKER_ROOT="/var/lib/docker"
  fi
  log "/dockerroot is absent; using regular Docker data path ${DOCKER_ROOT}"
}

detect_checkout() {
  if [[ "${KEEP_CHECKOUT}" == "1" || -n "${REPO_DIR}" ]]; then
    return
  fi

  local current="${USER_HOME}/infra-controller"
  local legacy="${USER_HOME}/ncx-infra-controller-core"
  if [[ -e "${current}" && -e "${legacy}" ]]; then
    die "both checkout paths exist; select one with --repo-dir"
  elif [[ -e "${current}" ]]; then
    REPO_DIR="${current}"
  elif [[ -e "${legacy}" ]]; then
    REPO_DIR="${legacy}"
  else
    REPO_DIR="${current}"
  fi
}

validate_checkout() {
  if [[ "${KEEP_CHECKOUT}" == "1" || ! -e "${REPO_DIR}" ]]; then
    return
  fi

  [[ "${REPO_DIR}" == "${USER_HOME}/"* ]] || \
    die "checkout must be a child of ${USER_HOME}: ${REPO_DIR}"
  [[ "${REPO_DIR}" != "${USER_HOME}" ]] || die "refusing to delete the user home"
  [[ -d "${REPO_DIR}/.git" && -f "${REPO_DIR}/devspace.yaml" ]] || \
    die "not an infra-controller checkout: ${REPO_DIR}"

  local origin
  origin="$(run_as_user git -C "${REPO_DIR}" remote get-url origin 2>/dev/null || true)"
  case "${origin}" in
    *infra-controller|*infra-controller.git) ;;
    *) die "unexpected checkout origin: ${origin:-unset}" ;;
  esac

  if [[ -n "$(run_as_user git -C "${REPO_DIR}" status --porcelain)" && \
    "${FORCE_DIRTY_CHECKOUT}" != "1" ]]; then
    if [[ "${APPLY}" == "1" ]]; then
      die "checkout is dirty; inspect it or pass --force-dirty-checkout"
    fi
    log "Warning: checkout is dirty; --apply will require --force-dirty-checkout"
  fi
}

validate_docker_root() {
  [[ "${DOCKER_ROOT}" == /* ]] || die "--docker-root must be absolute"
  case "${DOCKER_ROOT}" in
    /|/home|/var|/usr|/etc) die "unsafe Docker root: ${DOCKER_ROOT}" ;;
  esac
  [[ ! -e "${DOCKER_ROOT}" || -d "${DOCKER_ROOT}" ]] || \
    die "Docker root is not a directory: ${DOCKER_ROOT}"
}

validate_host_config() {
  if [[ "${KEEP_HOST_CONFIG}" == "1" ]]; then
    return
  fi

  if [[ -e /etc/docker/daemon.json ]]; then
    jq -e 'type == "object"' /etc/docker/daemon.json >/dev/null || \
      die "/etc/docker/daemon.json is not a JSON object"
    local configured_docker_root
    configured_docker_root="$(
      jq -r '."data-root" // empty' /etc/docker/daemon.json
    )"
    if [[ -n "${configured_docker_root}" && \
      "${configured_docker_root}" != "${DOCKER_ROOT}" ]]; then
      die "unexpected Docker data root: ${configured_docker_root}"
    fi
    if [[ "${configured_docker_root}" == "${DOCKER_ROOT}" ]]; then
      DOCKER_DATA_ROOT_IS_SETUP=1
    fi
  fi

  if [[ -e /etc/containerd/config.toml ]]; then
    local configured_root
    configured_root="$(sed -nE \
      's|^root[[:space:]]*=[[:space:]]*"([^"]+)".*$|\1|p' \
      /etc/containerd/config.toml)"
    if [[ "${configured_root}" == "${DOCKER_ROOT}/containerd" ]]; then
      CONTAINERD_ROOT_IS_SETUP=1
    elif [[ -z "${configured_root}" || \
      "${configured_root}" == "/var/lib/containerd" ]]; then
      CONTAINERD_ROOT_IS_SETUP=0
    else
      die "unexpected containerd root: ${configured_root:-unset}"
    fi
  fi

  local dropin
  for dropin in \
    /etc/systemd/system/docker.service.d/10-tls-compat.conf \
    /etc/systemd/system/containerd.service.d/10-tls-compat.conf; do
    if [[ -e "${dropin}" ]] && \
      ! cmp -s "${dropin}" <(
        printf '%s\n' '[Service]' 'Environment="GODEBUG=tlsmlkem=0"'
      ); then
      die "systemd drop-in contains unexpected content: ${dropin}"
    fi
  done
}

validate_host_confirmation() {
  if [[ "${APPLY}" != "1" ]]; then
    return
  fi
  [[ -n "${CONFIRM_HOST}" ]] || die "--apply requires --confirm-host"

  local short_name fqdn
  short_name="$(hostname)"
  fqdn="$(hostname -f 2>/dev/null || true)"
  if [[ "${CONFIRM_HOST}" != "${short_name}" && \
    "${CONFIRM_HOST}" != "${fqdn}" ]]; then
    die "hostname confirmation '${CONFIRM_HOST}' does not match ${short_name}/${fqdn}"
  fi
}

print_plan() {
  cat <<EOF
[reset-devspace-on-host] Reset plan for $(hostname -f 2>/dev/null || hostname):
  user:             ${DEV_USER}
  cluster:          kind-${CLUSTER_NAME}
  Docker data:      ${DOCKER_ROOT} (ALL images, containers, and volumes are
                    deleted, including any unrelated to NICo; path is preserved)
  checkout:         $(
    if [[ "${KEEP_CHECKOUT}" == "1" ]]; then
      printf 'preserved'
    else
      printf '%s' "${REPO_DIR}"
    fi
  )
  host config:      $(
    if [[ "${KEEP_HOST_CONFIG}" == "1" ]]; then
      printf 'preserved'
    else
      printf 'remove setup-owned overrides and docker membership'
    fi
  )
  user tools/state: ~/.local DevSpace tools, kube/Helm/DevSpace/Docker caches,
                    REST integration temp files, and shell environment block
  Docker packages:  preserved
  Docker data path: preserved after all Docker data is deleted
EOF
}

delete_kind_cluster() {
  if [[ -x "${USER_HOME}/.local/bin/kind" ]] && \
    run_as_user kind get clusters 2>/dev/null | grep -Fxq "${CLUSTER_NAME}"; then
    log "Deleting kind cluster ${CLUSTER_NAME}"
    run_as_user kind delete cluster --name "${CLUSTER_NAME}"
  else
    log "kind cluster ${CLUSTER_NAME} is already absent"
  fi
}

clear_docker_data() {
  log "Stopping Docker and containerd"
  systemctl stop docker.service docker.socket containerd.service \
    >/dev/null 2>&1 || true

  log "Removing ALL Docker images, containers, and volumes from ${DOCKER_ROOT}"
  rm -rf -- \
    "${DOCKER_ROOT}/buildkit" \
    "${DOCKER_ROOT}/containerd" \
    "${DOCKER_ROOT}/containers" \
    "${DOCKER_ROOT}/engine-id" \
    "${DOCKER_ROOT}/image" \
    "${DOCKER_ROOT}/network" \
    "${DOCKER_ROOT}/overlay2" \
    "${DOCKER_ROOT}/plugins" \
    "${DOCKER_ROOT}/runtimes" \
    "${DOCKER_ROOT}/swarm" \
    "${DOCKER_ROOT}/tmp" \
    "${DOCKER_ROOT}/trust" \
    "${DOCKER_ROOT}/volumes"
}

restore_host_config() {
  if [[ "${KEEP_HOST_CONFIG}" == "1" ]]; then
    log "Restarting Docker with preserved host configuration"
    systemctl start containerd.service docker.service
    wait_for_docker
    return
  fi

  log "Restoring Docker and containerd host configuration"
  if [[ "${DOCKER_DATA_ROOT_IS_SETUP}" == "1" ]]; then
    local daemon_tmp
    daemon_tmp="$(mktemp)"
    jq 'del(.["data-root"])' /etc/docker/daemon.json >"${daemon_tmp}"
    if jq -e 'length == 0' "${daemon_tmp}" >/dev/null; then
      rm -f -- /etc/docker/daemon.json
    else
      install -m 0644 "${daemon_tmp}" /etc/docker/daemon.json
    fi
    rm -f "${daemon_tmp}"
  fi
  rm -f -- \
    /etc/systemd/system/docker.service.d/10-tls-compat.conf \
    /etc/systemd/system/containerd.service.d/10-tls-compat.conf

  if [[ "${CONTAINERD_ROOT_IS_SETUP}" == "1" && \
    -e /etc/containerd/config.toml ]]; then
    local config_tmp
    config_tmp="$(mktemp)"
    sed -E \
      's|^root[[:space:]]*=.*$|root = "/var/lib/containerd"|' \
      /etc/containerd/config.toml >"${config_tmp}"
    install -m 0644 "${config_tmp}" /etc/containerd/config.toml
    rm -f "${config_tmp}"
  fi

  if id -nG "${DEV_USER}" | tr ' ' '\n' | grep -Fxq docker; then
    gpasswd -d "${DEV_USER}" docker >/dev/null
  fi

  systemctl daemon-reload
  systemctl enable --now containerd.service docker.service
  wait_for_docker
}

remove_shell_block() {
  local rc_file rc_tmp has_end mode
  local marker="# NICo DevSpace VM bootstrap"
  local end_marker="# end NICo DevSpace VM bootstrap"
  for rc_file in \
    "${USER_HOME}/.cshrc" \
    "${USER_HOME}/.bash_profile" \
    "${USER_HOME}/.bash_login" \
    "${USER_HOME}/.profile"; do
    [[ -e "${rc_file}" ]] || continue

    rc_tmp="$(mktemp)"
    mode="$(stat -c '%a' "${rc_file}")"
    has_end="$(awk -v marker="${marker}" -v end_marker="${end_marker}" '
      $0 == marker { saw_marker = 1; next }
      saw_marker && $0 == end_marker { print 1; exit }
    ' "${rc_file}")"
    has_end="${has_end:-0}"
    awk -v marker="${marker}" -v end_marker="${end_marker}" \
      -v has_end="${has_end}" '
      function flush_blanks() {
        printf "%s", blanks
        blanks = ""
      }
      in_block {
        if ((has_end && $0 == end_marker) || (!has_end && ++legacy_lines == 2)) {
          in_block = 0
        }
        next
      }
      $0 == marker {
        blanks = ""
        in_block = 1
        legacy_lines = 0
        next
      }
      $0 == "" {
        blanks = blanks $0 ORS
        next
      }
      {
        flush_blanks()
        print
      }
      END {
        if (!in_block) {
          flush_blanks()
        }
      }
    ' "${rc_file}" >"${rc_tmp}"
    if [[ -n "$(tr -d '[:space:]' <"${rc_tmp}")" ]]; then
      install -o "${DEV_USER}" -g "${USER_GROUP}" -m "${mode}" \
        "${rc_tmp}" "${rc_file}"
    else
      rm -f -- "${rc_file}"
    fi
    rm -f "${rc_tmp}"
  done
}

wait_for_docker() {
  local _attempt
  for _attempt in {1..30}; do
    if docker info >/dev/null 2>&1; then
      return
    fi
    sleep 1
  done
  die "Docker did not become ready"
}

remove_user_state() {
  log "Removing setup-owned tools and user state"

  if [[ -d "${USER_HOME}/.kube" ]]; then
    local remaining_contexts
    if remaining_contexts="$(
      run_as_user kubectl config get-contexts -o name 2>/dev/null
    )"; then
      if [[ -z "${remaining_contexts}" ]]; then
        rm -rf -- "${USER_HOME}/.kube"
      else
        log "Preserving ~/.kube because unrelated contexts remain"
      fi
    else
      log "Preserving ~/.kube because Kubernetes contexts could not be enumerated"
    fi
  fi

  rm -f -- \
    "${USER_HOME}/.local/bin/devspace" \
    "${USER_HOME}/.local/bin/helm" \
    "${USER_HOME}/.local/bin/kind" \
    "${USER_HOME}/.local/bin/kubectl"
  rm -rf -- \
    "${USER_HOME}/.cache/helm" \
    "${USER_HOME}/.config/helm" \
    "${USER_HOME}/.devspace" \
    "${USER_HOME}/.docker/buildx" \
    "${USER_HOME}/.docker/.token_seed" \
    "${USER_HOME}/.docker/.token_seed.lock" \
    "${USER_HOME}/Developer/_agent-tmp/devspace-rest"

  remove_shell_block

  rmdir "${USER_HOME}/.local/bin" "${USER_HOME}/.local" \
    "${USER_HOME}/.docker" \
    "${USER_HOME}/Developer/_agent-tmp" "${USER_HOME}/Developer" \
    2>/dev/null || true
}

remove_checkout() {
  if [[ "${KEEP_CHECKOUT}" == "1" || ! -e "${REPO_DIR}" ]]; then
    return
  fi
  log "Deleting checkout ${REPO_DIR}"
  rm -rf -- "${REPO_DIR}"
}

verify_reset() {
  log "Verifying reset"
  if docker ps -a --format '{{.Names}}' | grep -Fxq "${CLUSTER_NAME}-control-plane"; then
    die "kind control-plane container still exists"
  fi
  if [[ "${KEEP_CHECKOUT}" != "1" && -e "${REPO_DIR}" ]]; then
    die "checkout still exists: ${REPO_DIR}"
  fi
  if [[ -e "${USER_HOME}/.local/bin/devspace" ]]; then
    die "DevSpace binary still exists"
  fi
  log "Reset complete; Docker packages and ${DOCKER_ROOT} were preserved"
  if [[ -d "${DOCKER_ROOT}" ]]; then
    df -h / "${DOCKER_ROOT}"
  else
    df -h /
  fi
}

main() {
  resolve_docker_root
  detect_checkout
  validate_checkout
  validate_docker_root
  validate_host_config
  validate_host_confirmation
  print_plan

  if [[ "${APPLY}" != "1" ]]; then
    log "Dry run only; pass --apply and --confirm-host to execute"
    return
  fi

  delete_kind_cluster
  clear_docker_data
  restore_host_config
  remove_user_state
  remove_checkout
  verify_reset
}

main "$@"
