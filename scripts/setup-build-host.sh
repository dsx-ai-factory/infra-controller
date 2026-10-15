#!/usr/bin/env bash
set -euo pipefail

# Bootstrap an Ubuntu/Debian build host for building NICo container images and
# boot artifacts. This installs every prerequisite documented in
# docs/manuals/building_nico_containers.md in one step. It is idempotent --
# safe to re-run.
#
# Usage (from a fresh clone of the repo):
#   git clone git@github.com:NVIDIA/infra-controller.git
#   cd infra-controller
#   ./scripts/setup-build-host.sh        # or: make bootstrap
#
# A reboot (or at least a fresh login) is required afterwards so the docker
# group membership and the userns sysctl change take effect. Then build the
# images with `make images`.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

# --- 0. Host sanity -----------------------------------------------------------
# Building NICo containers requires an apt-based Linux host (Ubuntu 24.04
# recommended). macOS is not supported.
if [[ "$(uname -s)" != "Linux" ]] || ! command -v apt-get >/dev/null 2>&1; then
    echo "ERROR: build-host setup requires an apt-based Linux host (Ubuntu 24.04)." >&2
    echo "       Building NICo containers is not supported on macOS." >&2
    exit 1
fi

SUDO=""
if [[ "${EUID}" -ne 0 ]]; then
    SUDO="sudo"
fi
DOCKER_USER="${SUDO_USER:-$(id -un)}"

# --- 1. System packages -------------------------------------------------------
echo "=== [1/9] Installing system packages via apt ==="
${SUDO} apt-get update
${SUDO} env DEBIAN_FRONTEND=noninteractive apt-get install -y \
    build-essential cpio direnv mkosi uidmap curl file fakeroot git \
    docker.io docker-buildx sccache protobuf-compiler libopenipmi-dev \
    libudev-dev libboost-dev libgrpc-dev libprotobuf-dev libssl-dev \
    libtss2-dev kea-dev systemd-boot systemd-ukify jq zip

# --- 2. Rust toolchain --------------------------------------------------------
echo "=== [2/9] Installing rustup + Rust toolchain ==="
if command -v rustup >/dev/null 2>&1; then
    echo "rustup already installed -- skipping"
else
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi
# Make cargo available to the remaining steps in this shell.
if [[ -f "${HOME}/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    source "${HOME}/.cargo/env"
fi

# --- 3. direnv shell hook -----------------------------------------------------
echo "=== [3/9] Configuring the direnv shell hook ==="
_shell="$(basename "${SHELL:-bash}")"
case "${_shell}" in
    bash) _rc="${HOME}/.bashrc"; _hook='eval "$(direnv hook bash)"' ;;
    zsh)  _rc="${HOME}/.zshrc";  _hook='eval "$(direnv hook zsh)"' ;;
    *)    _rc="" ;;
esac
if [[ -n "${_rc}" ]]; then
    if grep -qF 'direnv hook' "${_rc}" 2>/dev/null; then
        echo "direnv hook already present in ${_rc} -- skipping"
    else
        echo "${_hook}" >> "${_rc}"
        echo "Added direnv hook to ${_rc}"
    fi
else
    echo "Unrecognized shell '${_shell}'. Add the direnv hook manually:"
    echo "  https://direnv.net/docs/hook.html"
fi

# --- 4. Allow direnv for this repo --------------------------------------------
echo "=== [4/9] Allowing direnv for this repo ==="
direnv allow .

# --- 5. Git submodules (mkosi + ipxe) -----------------------------------------
# The pinned mkosi and ipxe sources live under pxe/ as git submodules.
echo "=== [5/9] Fetching git submodules (mkosi, ipxe) ==="
git submodule update --init --recursive

# --- 6. Docker socket ---------------------------------------------------------
echo "=== [6/9] Enabling the docker socket ==="
${SUDO} systemctl enable --now docker.socket

# --- 7. Cross-architecture container support ---------------------------------
echo "=== [7/9] Registering cross-architecture container support ==="
${SUDO} docker run --privileged --rm tonistiigi/binfmt@sha256:400a4873b838d1b89194d982c45e5fb3cda4593fbfd7e08a02e76b03b21166f0 --install all

# --- 8. Cargo build tooling ---------------------------------------------------
echo "=== [8/9] Installing cargo build tooling (cargo-make, cargo-cache) ==="
command -v cargo-make  >/dev/null 2>&1 || cargo install cargo-make
command -v cargo-cache >/dev/null 2>&1 || cargo install cargo-cache

# --- 9. Host configuration ----------------------------------------------------
echo "=== [9/9] Configuring host (unprivileged userns + docker group) ==="
echo "kernel.apparmor_restrict_unprivileged_userns=0" \
    | ${SUDO} tee /etc/sysctl.d/99-userns.conf >/dev/null
${SUDO} usermod -aG docker "${DOCKER_USER}"

echo ""
echo "================================================================================"
echo "Build host bootstrap complete."
echo ""
echo "  Reboot (or log out and back in) so the docker group membership and the"
echo "  userns sysctl change take effect, then build the images from the repo root:"
echo ""
echo "      make images"
echo "================================================================================"
