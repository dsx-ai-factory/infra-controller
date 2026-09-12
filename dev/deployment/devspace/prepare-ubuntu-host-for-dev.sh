#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Prepare an x86_64 or aarch64 Ubuntu VM to run tests and stack for NICo.
# You should be able to execute the following tasks:
#   - run `cargo test`
#   - run `make -C rest-api test`
#   - bring up the devspace stack
#
# Toolchain versions are read from the checkout when possible so the VM follows the
# repository instead of the historical versions in those scripts.

set -euo pipefail

DEV_USER=""
REPO_DIR=""
SKIP_CORE_POSTGRES=0

PROTO_VERSION="25.7"
PROTO_SHA256_X86_64="877408bab02767938d1e5555f11c39dfe05e96f2a9571bc59dd2639f33d9954e"
PROTO_SHA256_AARCH_64="58135d20be2831d9ca5a39675f4499f9cbad8b44f9c3d814287c0b543155a812"
GRPCURL_VERSION="1.8.7"
VAULT_VERSION="1.21.4-1"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=versions.env
source "${SCRIPT_DIR}/versions.env"
CORE_POSTGRES_CONTAINER="nico-core-test-postgres"
SCRIPT_START_SECONDS="${SECONDS}"

format_duration() {
  local total_seconds="$1"
  printf '%02d:%02d:%02d' \
    "$((total_seconds / 3600))" \
    "$(((total_seconds % 3600) / 60))" \
    "$((total_seconds % 60))"
}

log() {
  printf '[prepare-ubuntu-host-for-dev] %s\n' "$*"
}

die() {
  printf '[prepare-ubuntu-host-for-dev] error: %s\n' "$*" >&2
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
  prepare-ubuntu-host-for-dev.sh [options]

Options:
  --user USER          Developer account to configure. Defaults to the user
                       that invoked sudo.
  --repo-dir PATH      infra-controller checkout. Defaults to the checkout
                       containing this script, then ~/infra-controller.
  --skip-core-postgres Do not create the PostgreSQL container used by Rust tests.
  -h, --help           Show this help.

Examples:
  dev/deployment/devspace/prepare-ubuntu-host-for-dev.sh
  sudo dev/deployment/devspace/prepare-ubuntu-host-for-dev.sh \
    --user foobar --repo-dir /home/foobar/infra-controller
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
    --skip-core-postgres)
      SKIP_CORE_POSTGRES=1
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
    DEV_USER="$(id -un)"
  fi
  sudo_args=(bash "$0" --user "${DEV_USER}")
  if [[ -n "${REPO_DIR}" ]]; then
    sudo_args+=(--repo-dir "${REPO_DIR}")
  fi
  if [[ "${SKIP_CORE_POSTGRES}" == "1" ]]; then
    sudo_args+=(--skip-core-postgres)
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
[[ -n "${USER_HOME}" && "${USER_HOME}" != "/" ]] || \
  die "invalid home directory for ${DEV_USER}"

if [[ -z "${REPO_DIR}" ]]; then
  script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
  possible_repo="$(cd -- "${script_dir}/../../.." && pwd)"
  if [[ -f "${possible_repo}/Cargo.toml" && \
    -f "${possible_repo}/rest-api/go.mod" ]]; then
    REPO_DIR="${possible_repo}"
  else
    REPO_DIR="${USER_HOME}/infra-controller"
  fi
fi
[[ -f "${REPO_DIR}/Cargo.toml" ]] || die "Cargo workspace not found: ${REPO_DIR}"
[[ -f "${REPO_DIR}/rest-api/go.mod" ]] || die "REST Go module not found: ${REPO_DIR}"

[[ -r /etc/os-release ]] || die "cannot determine the operating system"
# shellcheck source=/dev/null
source /etc/os-release
[[ "${ID:-}" == "ubuntu" ]] || die "this script currently supports Ubuntu only"

case "$(uname -m)" in
  x86_64)
    MACHINE_ARCH="x86_64"
    GO_ARCH="amd64"
    PROTO_ARCH="x86_64"
    GRPCURL_ARCH="x86_64"
    ;;
  aarch64|arm64)
    MACHINE_ARCH="aarch64"
    GO_ARCH="arm64"
    PROTO_ARCH="aarch_64"
    GRPCURL_ARCH="arm64"
    ;;
  *)
    die "unsupported architecture: $(uname -m)"
    ;;
esac

RUST_VERSION="$(
  sed -nE 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"([^"]+)".*$/\1/p' \
    "${REPO_DIR}/rust-toolchain.toml" | head -n 1
)"
GO_VERSION="$(
  sed -nE 's/^go[[:space:]]+([^[:space:]]+).*$/\1/p' \
    "${REPO_DIR}/rest-api/go.mod" | head -n 1
)"
[[ -n "${RUST_VERSION}" ]] || die "could not read Rust version"
[[ -n "${GO_VERSION}" ]] || die "could not read Go version"

# Commands run as the developer must start from a directory that account can
# traverse. This also ensures rustup discovers the repository toolchain when
# the setup script was launched with sudo from a root-only directory.
cd -- "${REPO_DIR}"

DEV_PATH="${USER_HOME}/.cargo/bin:/usr/local/go/bin:/usr/local/protobuf/bin:${USER_HOME}/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
DATABASE_URL="postgresql://postgres:admin@localhost"
OPENSSL_COMPAT_CONFIG="${USER_HOME}/.config/nico/openssl-compat.cnf"

run_as_user() {
  runuser -u "${DEV_USER}" -- env \
    HOME="${USER_HOME}" \
    USER="${DEV_USER}" \
    PATH="${DEV_PATH}" \
    OPENSSL_CONF="${OPENSSL_COMPAT_CONFIG}" \
    CARGO_HTTP_LOW_SPEED_LIMIT="1" \
    CARGO_HTTP_TIMEOUT="120" \
    CARGO_NET_GIT_FETCH_WITH_CLI="true" \
    CARGO_NET_RETRY="5" \
    GODEBUG="tlsmlkem=0" \
    TESTDB_USER="postgres" \
    TESTDB_PASSWORD="admin" \
    TESTDB_HOST="localhost" \
    DATABASE_URL="${DATABASE_URL}" \
    REPO_ROOT="${REPO_DIR}" \
    RUSTUP_TOOLCHAIN="${RUST_VERSION}" \
    RUSTC_WRAPPER="sccache" \
    "$@"
}

configure_openssl_tls() {
  log "Configuring OpenSSL"
  install -o "${DEV_USER}" -g "${USER_GROUP}" -m 0755 \
    -d "${USER_HOME}/.config"
  install -o "${DEV_USER}" -g "${USER_GROUP}" -m 0755 \
    -d "$(dirname "${OPENSSL_COMPAT_CONFIG}")"
  cat >"${OPENSSL_COMPAT_CONFIG}" <<'EOF'
# Managed by prepare-ubuntu-host-for-dev.sh.
#
# OpenSSL 3.5 sends an X25519MLKEM768 key share by default. Some network paths
# drop that larger TLS ClientHello, so use classical groups.
openssl_conf = openssl_init

[openssl_init]
ssl_conf = ssl_configuration

[ssl_configuration]
system_default = tls_system_default

[tls_system_default]
Groups = X25519:P-256:P-384
EOF
  chown "${DEV_USER}:${USER_GROUP}" "${OPENSSL_COMPAT_CONFIG}"
  chmod 0644 "${OPENSSL_COMPAT_CONFIG}"
}

install_base_packages() {
  log "Installing native build and test dependencies for ${MACHINE_ARCH}"
  apt-get update

  local packages=(
    apparmor
    apparmor-utils
    automake
    build-essential
    ca-certificates
    clang
    cmake
    curl
    dosfstools
    fdisk
    git
    gnupg
    ipmitool
    iproute2
    iputils-ping
    jq
    just
    kea-dev
    kea-dhcp4-server
    kea-dhcp6-server
    less
    libaio-dev
    libboost-dev
    libgrpc++-dev
    libgrpc-dev
    libopenipmi-dev
    libprotobuf-dev
    libssh-dev
    libssl-dev
    libtss2-dev
    libudev-dev
    lld
    make
    openipmi
    openssh-client
    pandoc
    pkg-config
    postgresql-client
    protobuf-compiler-grpc
    rsync
    sccache
    sudo
    tpm2-tools
    unzip
    wget
  )
  if [[ "${MACHINE_ARCH}" == "aarch64" ]]; then
    packages+=(mold)
  fi
  local virtualization
  virtualization="$(systemd-detect-virt --vm 2>/dev/null || true)"
  if [[ "${virtualization}" == "vmware" ]]; then
    packages+=(open-vm-tools)
  fi

  DEBIAN_FRONTEND=noninteractive apt-get install -y "${packages[@]}"
}

install_docker() {
  if command -v docker >/dev/null 2>&1 && \
    command -v containerd >/dev/null 2>&1 && \
    docker buildx version >/dev/null 2>&1; then
    log "Reusing installed Docker"
  else
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
  fi

  install -m 0755 -d /etc/systemd/system/docker.service.d
  cat >/etc/systemd/system/docker.service.d/10-tls-compat.conf <<'EOF'
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

install_go() {
  local installed=""
  if [[ -x /usr/local/go/bin/go ]]; then
    installed="$(/usr/local/go/bin/go env GOVERSION 2>/dev/null || true)"
  fi
  if [[ "${installed}" == "go${GO_VERSION}" ]]; then
    log "Reusing Go ${GO_VERSION}"
    return
  fi

  log "Installing Go ${GO_VERSION} for linux/${GO_ARCH}"
  local work_dir archive url metadata_url expected actual
  work_dir="$(mktemp -d)"
  archive="go${GO_VERSION}.linux-${GO_ARCH}.tar.gz"
  url="https://go.dev/dl/${archive}"
  metadata_url="https://go.dev/dl/?mode=json&include=all"
  curl -fsSL "${url}" -o "${work_dir}/${archive}"
  expected="$(
    curl -fsSL "${metadata_url}" |
      jq -er --arg archive "${archive}" \
        'first(.[] | .files[] | select(.filename == $archive) | .sha256)'
  )" || die "could not retrieve the checksum for ${archive}"
  [[ "${expected}" =~ ^[[:xdigit:]]{64}$ ]] || \
    die "invalid checksum metadata for ${archive}"
  actual="$(sha256sum "${work_dir}/${archive}" | awk '{print $1}')"
  [[ "${actual}" == "${expected}" ]] || \
    die "Go archive checksum failed for ${archive}"

  rm -rf -- /usr/local/go
  tar -C /usr/local -xzf "${work_dir}/${archive}"
  rm -rf -- "${work_dir}"
}

install_rust() {
  local install_args=(toolchain install "${RUST_VERSION}" --profile minimal)

  if [[ ! -x "${USER_HOME}/.cargo/bin/rustup" ]]; then
    log "Installing rustup for ${DEV_USER}"
    run_as_user bash -c \
      'curl --proto "=https" --tlsv1.2 -fsS https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal'
  fi

  if ! run_as_user rustup which --toolchain "${RUST_VERSION}" rustc \
    >/dev/null 2>&1; then
    log "Rust ${RUST_VERSION} is missing or incomplete; forcing repair"
    install_args+=(--force)
  fi

  log "Installing Rust ${RUST_VERSION}"
  run_as_user rustup "${install_args[@]}"
  run_as_user rustup which --toolchain "${RUST_VERSION}" rustc >/dev/null
  run_as_user rustup component add \
    --toolchain "${RUST_VERSION}" \
    clippy \
    rustfmt
}

install_protoc() {
  local current=""
  if [[ -x /usr/local/protobuf/bin/protoc ]]; then
    current="$(/usr/local/protobuf/bin/protoc --version 2>/dev/null || true)"
  fi
  if [[ "${current}" == "libprotoc ${PROTO_VERSION}" ]]; then
    log "Reusing protoc ${PROTO_VERSION}"
    return
  fi

  log "Installing protoc ${PROTO_VERSION} for ${PROTO_ARCH}"
  local work_dir archive url expected actual
  work_dir="$(mktemp -d)"
  archive="protoc-${PROTO_VERSION}-linux-${PROTO_ARCH}.zip"
  url="https://github.com/protocolbuffers/protobuf/releases/download/v${PROTO_VERSION}/${archive}"
  case "${PROTO_ARCH}" in
    x86_64) expected="${PROTO_SHA256_X86_64}" ;;
    aarch_64) expected="${PROTO_SHA256_AARCH_64}" ;;
    *) die "no pinned protoc checksum for ${PROTO_ARCH}" ;;
  esac
  curl -fsSL "${url}" -o "${work_dir}/${archive}"
  actual="$(sha256sum "${work_dir}/${archive}" | awk '{print $1}')"
  [[ "${actual}" == "${expected}" ]] || \
    die "protoc archive checksum failed for ${archive}"
  rm -rf -- /usr/local/protobuf
  mkdir -p /usr/local/protobuf
  unzip -q "${work_dir}/${archive}" -d /usr/local/protobuf
  rm -rf -- "${work_dir}"
}

install_grpcurl() {
  local current=""
  if command -v grpcurl >/dev/null 2>&1; then
    current="$(grpcurl --version 2>/dev/null | awk '{sub(/^v/, "", $2); print $2}' || true)"
  fi
  if [[ "${current}" == "${GRPCURL_VERSION}" ]]; then
    log "Reusing grpcurl ${GRPCURL_VERSION}"
    return
  fi

  log "Installing grpcurl ${GRPCURL_VERSION} for ${GRPCURL_ARCH}"
  local work_dir archive url checksum_url expected actual
  work_dir="$(mktemp -d)"
  archive="grpcurl_${GRPCURL_VERSION}_linux_${GRPCURL_ARCH}.tar.gz"
  url="https://github.com/fullstorydev/grpcurl/releases/download/v${GRPCURL_VERSION}/${archive}"
  checksum_url="https://github.com/fullstorydev/grpcurl/releases/download/v${GRPCURL_VERSION}/grpcurl_${GRPCURL_VERSION}_checksums.txt"
  curl -fsSL "${url}" -o "${work_dir}/${archive}"
  expected="$(curl -fsSL "${checksum_url}" | awk -v archive="${archive}" \
    '$2 == archive {print $1; exit}')"
  [[ "${expected}" =~ ^[[:xdigit:]]{64}$ ]] || \
    die "could not retrieve the checksum for ${archive}"
  actual="$(sha256sum "${work_dir}/${archive}" | awk '{print $1}')"
  [[ "${actual}" == "${expected}" ]] || \
    die "grpcurl archive checksum failed for ${archive}"
  tar -xzf "${work_dir}/${archive}" -C "${work_dir}" grpcurl
  install -m 0755 "${work_dir}/grpcurl" /usr/local/bin/grpcurl
  rm -rf -- "${work_dir}"
}

install_vault() {
  local vault_path installed_version
  vault_path="$(command -v vault 2>/dev/null || true)"
  installed_version="$(
    dpkg-query -W -f='${Version}' vault 2>/dev/null || true
  )"
  if [[ -n "${vault_path}" && \
    "$(readlink -f "${vault_path}")" == "/usr/bin/vault" && \
    "${installed_version}" == "${VAULT_VERSION}" ]]; then
    log "Reusing installed Vault"
    return
  fi

  log "Installing Vault ${VAULT_VERSION}"
  curl -fsSL https://apt.releases.hashicorp.com/gpg |
    gpg --dearmor --yes -o /usr/share/keyrings/hashicorp-archive-keyring.gpg
  cat >/etc/apt/sources.list.d/hashicorp.sources <<EOF
Types: deb
URIs: https://apt.releases.hashicorp.com
Suites: ${VERSION_CODENAME}
Components: main
  Signed-By: /usr/share/keyrings/hashicorp-archive-keyring.gpg
EOF
  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y \
    --allow-change-held-packages "vault=${VAULT_VERSION}"
  apt-mark hold vault >/dev/null

  vault_path="$(command -v vault 2>/dev/null || true)"
  installed_version="$(
    dpkg-query -W -f='${Version}' vault 2>/dev/null || true
  )"
  [[ -n "${vault_path}" && \
    "$(readlink -f "${vault_path}")" == "/usr/bin/vault" && \
    "${installed_version}" == "${VAULT_VERSION}" ]] || \
    die "Vault ${VAULT_VERSION} was installed but is not the active executable"
}

configure_kea_apparmor() {
  local canonical_repo_dir
  canonical_repo_dir="$(realpath "${REPO_DIR}")"
  install -m 0755 -d /etc/apparmor.d/local

  local daemon profile local_profile
  for daemon in kea-dhcp4 kea-dhcp6; do
    profile="/etc/apparmor.d/usr.sbin.${daemon}"
    [[ -e "${profile}" ]] || continue

    log "Allowing ${daemon} tests to use temporary files and Cargo-built hooks"
    local_profile="/etc/apparmor.d/local/usr.sbin.${daemon}"
    {
      printf '# Managed by prepare-ubuntu-host-for-dev.sh\n'
      printf '/tmp/** rwk,\n'
      printf '%s/target/debug/*.so mr,\n' "${REPO_DIR}"
      printf '%s/target/debug/deps/*.so mr,\n' "${REPO_DIR}"
      if [[ "${canonical_repo_dir}" != "${REPO_DIR}" ]]; then
        printf '%s/target/debug/*.so mr,\n' "${canonical_repo_dir}"
        printf '%s/target/debug/deps/*.so mr,\n' "${canonical_repo_dir}"
      fi
    } >"${local_profile}"
    apparmor_parser -r "${profile}"
  done
}

configure_k3s_kubeconfig() {
  local source_config="/etc/rancher/k3s/k3s.yaml"
  local user_kube_dir="${USER_HOME}/.kube"
  local user_config="${user_kube_dir}/config"
  [[ -f "${source_config}" ]] || return 0

  if run_as_user test -r "${user_config}"; then
    return
  fi

  if [[ -L "${user_config}" ]]; then
    local link_target
    link_target="$(readlink -f "${user_config}" || true)"
    if [[ "${link_target}" != "${source_config}" ]]; then
      log "Preserving unrelated kubeconfig symlink ${user_config}"
      return
    fi
    unlink "${user_config}"
  elif [[ -e "${user_config}" ]]; then
    log "Preserving existing unreadable kubeconfig ${user_config}"
    return
  fi

  log "Installing a private kubeconfig copy for the existing k3s cluster"
  install -o "${DEV_USER}" -g "${USER_GROUP}" -m 0700 -d "${user_kube_dir}"
  install -o "${DEV_USER}" -g "${USER_GROUP}" -m 0600 \
    "${source_config}" "${user_config}"
}

update_shell_file() {
  local rc_file="$1"
  local shell_kind="$2"
  local begin="# >>> NICo prepare-ubuntu-host-for-dev >>>"
  local end="# <<< NICo prepare-ubuntu-host-for-dev <<<"
  local work_file mode="0644"
  work_file="$(mktemp)"

  if [[ -e "${rc_file}" ]]; then
    mode="$(stat -c '%a' "${rc_file}")"
    sed "/^${begin}$/,/^${end}$/d" "${rc_file}" >"${work_file}"
  fi

  {
    printf '\n%s\n' "${begin}"
    if [[ "${shell_kind}" == "csh" ]]; then
      # shellcheck disable=SC2016 # Expand these in the developer's login shell.
      printf 'setenv PATH "$HOME/.cargo/bin:/usr/local/go/bin:/usr/local/protobuf/bin:$HOME/go/bin:$PATH"\n'
      # shellcheck disable=SC2016 # Expand $HOME in the developer's login shell.
      printf 'setenv OPENSSL_CONF "$HOME/.config/nico/openssl-compat.cnf"\n'
      printf 'setenv CARGO_HTTP_LOW_SPEED_LIMIT 1\n'
      printf 'setenv CARGO_HTTP_TIMEOUT 120\n'
      printf 'setenv CARGO_NET_GIT_FETCH_WITH_CLI true\n'
      printf 'setenv CARGO_NET_RETRY 5\n'
      printf 'setenv GODEBUG tlsmlkem=0\n'
      printf 'setenv TESTDB_USER postgres\n'
      printf 'setenv TESTDB_PASSWORD admin\n'
      printf 'setenv TESTDB_HOST localhost\n'
      printf 'setenv DATABASE_URL postgresql://postgres:admin@localhost\n'
      printf 'setenv REPO_ROOT "%s"\n' "${REPO_DIR}"
      printf 'setenv RUSTC_WRAPPER sccache\n'
      if run_as_user test -r "${USER_HOME}/.kube/config"; then
        # shellcheck disable=SC2016 # Expand $HOME in the developer's login shell.
        printf 'setenv KUBECONFIG "$HOME/.kube/config"\n'
      fi
    else
      # shellcheck disable=SC2016 # Expand these in the developer's login shell.
      printf 'export PATH="$HOME/.cargo/bin:/usr/local/go/bin:/usr/local/protobuf/bin:$HOME/go/bin:$PATH"\n'
      # shellcheck disable=SC2016 # Expand $HOME in the developer's login shell.
      printf 'export OPENSSL_CONF="$HOME/.config/nico/openssl-compat.cnf"\n'
      printf 'export CARGO_HTTP_LOW_SPEED_LIMIT=1\n'
      printf 'export CARGO_HTTP_TIMEOUT=120\n'
      printf 'export CARGO_NET_GIT_FETCH_WITH_CLI=true\n'
      printf 'export CARGO_NET_RETRY=5\n'
      printf 'export GODEBUG=tlsmlkem=0\n'
      printf 'export TESTDB_USER=postgres\n'
      printf 'export TESTDB_PASSWORD=admin\n'
      printf 'export TESTDB_HOST=localhost\n'
      printf 'export DATABASE_URL=postgresql://postgres:admin@localhost\n'
      printf 'export REPO_ROOT="%s"\n' "${REPO_DIR}"
      printf 'export RUSTC_WRAPPER=sccache\n'
      if run_as_user test -r "${USER_HOME}/.kube/config"; then
        # shellcheck disable=SC2016 # Expand $HOME in the developer's login shell.
        printf 'export KUBECONFIG="$HOME/.kube/config"\n'
      fi
    fi
    printf '%s\n' "${end}"
  } >>"${work_file}"

  install -o "${DEV_USER}" -g "${USER_GROUP}" -m "${mode}" \
    "${work_file}" "${rc_file}"
  rm -f "${work_file}"
}

configure_shell() {
  local login_shell rc_file candidate
  login_shell="$(getent passwd "${DEV_USER}" | cut -d: -f7)"
  case "${login_shell}" in
    *csh) update_shell_file "${USER_HOME}/.cshrc" csh ;;
    *)
      rc_file="${USER_HOME}/.profile"
      for candidate in .bash_profile .bash_login .profile; do
        if [[ -e "${USER_HOME}/${candidate}" ]]; then
          rc_file="${USER_HOME}/${candidate}"
          break
        fi
      done
      update_shell_file "${rc_file}" sh
      ;;
  esac
}

pull_postgres_images() {
  log "Caching PostgreSQL images used by both test suites"
  local image attempt
  for image in "${CORE_POSTGRES_IMAGE}" "${REST_POSTGRES_IMAGE}"; do
    for attempt in {1..6}; do
      if run_as_user docker pull "${image}"; then
        break
      fi
      if [[ "${attempt}" == "6" ]]; then
        die "could not pull ${image}"
      fi
      sleep $((attempt * 10))
    done
  done
}

start_core_postgres() {
  if [[ "${SKIP_CORE_POSTGRES}" == "1" ]]; then
    return
  fi

  local container_exists=0 port_bindings=""
  if run_as_user docker container inspect "${CORE_POSTGRES_CONTAINER}" \
    >/dev/null 2>&1; then
    container_exists=1
    port_bindings="$(run_as_user docker container inspect \
      --format '{{json .HostConfig.PortBindings}}' \
      "${CORE_POSTGRES_CONTAINER}" 2>/dev/null || true)"
    if ! jq -e '
      .["5432/tcp"] | type == "array" and length == 1 and
      all(.[]; .HostIp == "127.0.0.1" and .HostPort == "5432")
    ' <<<"${port_bindings}" >/dev/null 2>&1; then
      log "Recreating ${CORE_POSTGRES_CONTAINER} with a loopback-only port binding"
      run_as_user docker rm -f "${CORE_POSTGRES_CONTAINER}" >/dev/null
      container_exists=0
    fi
  fi

  if [[ "${container_exists}" == "1" ]]; then
    log "Starting existing ${CORE_POSTGRES_CONTAINER}"
    run_as_user docker start "${CORE_POSTGRES_CONTAINER}" >/dev/null
  else
    log "Starting PostgreSQL for Rust tests on localhost:5432"
    run_as_user docker run -d \
      --restart unless-stopped \
      --name "${CORE_POSTGRES_CONTAINER}" \
      -p 127.0.0.1:5432:5432 \
      -e POSTGRES_USER=postgres \
      -e POSTGRES_PASSWORD=admin \
      -e POSTGRES_DB=forgetest \
      "${CORE_POSTGRES_IMAGE}" \
      -c fsync=off \
      -c synchronous_commit=off \
      -c full_page_writes=off \
      >/dev/null
  fi

  local _attempt
  for _attempt in {1..60}; do
    if run_as_user docker exec "${CORE_POSTGRES_CONTAINER}" \
      pg_isready -U postgres -d forgetest >/dev/null 2>&1; then
      return
    fi
    sleep 1
  done
  die "Core test PostgreSQL did not become ready"
}

fetch_dependencies() {
  log "Fetching Rust dependencies"
  run_as_user cargo fetch --locked --manifest-path "${REPO_DIR}/Cargo.toml"

  log "Fetching REST Go dependencies"
  # shellcheck disable=SC2016 # $1 is expanded by the target user's shell.
  run_as_user bash -c 'cd "$1/rest-api" && go mod download' _ "${REPO_DIR}"
}

verify_setup() {
  log "Verifying toolchains and test services"
  run_as_user rustc --version
  run_as_user cargo --version
  run_as_user go version
  run_as_user protoc --version
  run_as_user grpcurl --version
  run_as_user vault version
  run_as_user docker version --format '{{.Client.Version}}'
  run_as_user cargo metadata \
    --no-deps \
    --format-version 1 \
    --manifest-path "${REPO_DIR}/Cargo.toml" \
    >/dev/null

  if [[ "${SKIP_CORE_POSTGRES}" != "1" ]]; then
    run_as_user docker exec "${CORE_POSTGRES_CONTAINER}" \
      pg_isready -U postgres -d forgetest
  fi
}

show_summary() {
  cat <<EOF

[prepare-ubuntu-host-for-dev] Setup complete for ${MACHINE_ARCH}.

Start a fresh login shell, then run:

  cd ${REPO_DIR}
  cargo test
  make -C rest-api test

Rust tests use ${CORE_POSTGRES_CONTAINER} on localhost:5432.
REST tests manage their own project-test container on localhost:30432.
EOF
}

main() {
  configure_openssl_tls
  install_base_packages
  install_docker
  install_go
  install_rust
  install_protoc
  install_grpcurl
  install_vault
  configure_kea_apparmor
  configure_k3s_kubeconfig
  configure_shell
  pull_postgres_images
  start_core_postgres
  fetch_dependencies
  verify_setup
  show_summary
}

main "$@"
