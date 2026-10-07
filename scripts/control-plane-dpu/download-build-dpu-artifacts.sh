#!/usr/bin/env bash
# build-dpu-artifacts.sh
#
# Downloads DPU artifacts for a given DOCA / HBN version.
# All artifacts are written to the current working directory.
#
#   1. BFB file                      -- downloaded as-is, no modification
#   2. doca_hbn.tar.gz               -- HBN container image (linux/arm64) saved to tarball
#   3. doca_container_configs.zip.gz -- HBN configs + scripts bundle from NGC
#   4. DOCA host package (.deb)      -- optional; downloaded from --doca-host-url
#
# Mirrors Makefile.toml tasks:
#   bfb-download, bfb-hbn-pull, bfb-hbn-export,
#   mkdir-hbn-folder-in-bfb, download-hbn-installer-to-bfb,
#   mv-hbn-configs-to-bfb, mv-hbn-scripts-to-bfb, cleanup-hbn-scripts
#
# Usage:
#   ./build-dpu-artifacts.sh --doca-version 3.2.2 --bfb-build 125 --bfb-release 26.02
#
# Required:
#   --doca-version VERSION      DOCA platform version, e.g. 3.2.2
#   --hbn-container-tag TAG     Exact NGC container tag for the HBN image, e.g.
#                               3.2.2-doca3.2.2 or 2.4.2-doca2.9.2-32
#                               (check https://catalog.ngc.nvidia.com for the tag)
#   --bfb-build NUMBER          BFB build number
#   --bfb-release VERSION       BFB release string
#   --doca-host-url URL         Full URL to DOCA host .deb package
#   --rshim-url URL             Full URL to rshim .deb package (from GitHub releases)
#                               e.g. https://github.com/Mellanox/rshim-user-space/releases/download/rshim-2.3.1/rshim_2.3.1_amd64.deb
#   --libfuse2-url URL          Full URL to libfuse2t64 .deb package (rshim dependency)
#                               e.g. http://archive.ubuntu.com/ubuntu/pool/universe/f/fuse/libfuse2t64_2.9.9-8.1build1_amd64.deb
#
# Optional:
#   --doca-hbn-version VERSION  HBN version if different from DOCA, used for the
#                               NGC config bundle URL (default: same as --doca-version)
#   --bfb-url URL               Base CDN URL for BFB download
#                               (default: https://content.mellanox.com/BlueField/BFBs/Ubuntu22.04)
#   --hbn-config-url URL        Full NGC files API URL for HBN config bundle
#                               (default: https://api.ngc.nvidia.com/v2/resources/org/nvidia/team/doca/doca_hbn/<doca-hbn-version>/files)
#   -h, --help                  Show this help
#
# Dependencies: wget, docker, curl, jq, base64, xxd, sha256sum, zip, gzip


set -euo pipefail

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
DOCA_VERSION=""
DOCA_HBN_VERSION=""
HBN_CONTAINER_TAG=""
BFB_BUILD=""
BFB_RELEASE=""
BFB_BASE_URL="https://content.mellanox.com/BlueField/BFBs/Ubuntu22.04"
HBN_CONFIG_URL=""
DOCA_HOST_URL=""
RSHIM_URL=""
LIBFUSE2_URL=""

usage() {
  grep '^#' "$0" | grep -v '#!/' | sed 's/^# \{0,1\}//'
  exit 1
}

die()  { echo; echo "ERROR: $*" >&2; exit 1; }
log()  { echo "[$(date '+%H:%M:%S')] $*"; }
step() { echo; echo "------------------------------------------------------------"; echo "[$(date '+%H:%M:%S')] $*"; echo "------------------------------------------------------------"; }
ok()   { echo "[$(date '+%H:%M:%S')] OK: $*"; }

# sha256_of <file> → prints hex digest; works on macOS (shasum) and Linux (sha256sum)
sha256_of() {
  if command -v sha256sum &>/dev/null && sha256sum --version &>/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

# b64decode: reads stdin, writes decoded bytes to stdout; works on macOS and Linux.
# Normalises URL-safe base64 (- → +, _ → /) before decoding, and adds padding
# if missing — both are common in NGC API responses.
b64decode() {
  local input
  input=$(cat | tr '_-' '/+')
  local rem=$(( ${#input} % 4 ))
  if [[ $rem -eq 2 ]]; then input="${input}=="; elif [[ $rem -eq 3 ]]; then input="${input}="; fi
  if [[ "$(uname)" == "Darwin" ]]; then
    printf '%s' "$input" | base64 -D
  else
    printf '%s' "$input" | base64 -d
  fi
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
  case "$1" in
    --doca-version)     [[ -z "${2:-}" ]] && die "$1 requires a value"; DOCA_VERSION="$2";     shift 2 ;;
    --doca-hbn-version) [[ -z "${2:-}" ]] && die "$1 requires a value"; DOCA_HBN_VERSION="$2"; shift 2 ;;
    --hbn-container-tag) [[ -z "${2:-}" ]] && die "$1 requires a value"; HBN_CONTAINER_TAG="$2"; shift 2 ;;
    --bfb-build)        [[ -z "${2:-}" ]] && die "$1 requires a value"; BFB_BUILD="$2";        shift 2 ;;
    --bfb-release)      [[ -z "${2:-}" ]] && die "$1 requires a value"; BFB_RELEASE="$2";      shift 2 ;;
    --bfb-url)          [[ -z "${2:-}" ]] && die "$1 requires a value"; BFB_BASE_URL="$2";     shift 2 ;;
    --hbn-config-url)   [[ -z "${2:-}" ]] && die "$1 requires a value"; HBN_CONFIG_URL="$2";   shift 2 ;;
    --doca-host-url)    [[ -z "${2:-}" ]] && die "$1 requires a value"; DOCA_HOST_URL="$2";    shift 2 ;;
    --rshim-url)        [[ -z "${2:-}" ]] && die "$1 requires a value"; RSHIM_URL="$2";        shift 2 ;;
    --libfuse2-url)     [[ -z "${2:-}" ]] && die "$1 requires a value"; LIBFUSE2_URL="$2";     shift 2 ;;
    -h|--help) usage ;;
    *) die "Unknown option: $1" ;;
  esac
done

[[ -z "$DOCA_VERSION" ]]      && die "--doca-version is required"
[[ -z "$HBN_CONTAINER_TAG" ]] && die "--hbn-container-tag is required"
[[ -z "$BFB_BUILD" ]]         && die "--bfb-build is required"
[[ -z "$BFB_RELEASE" ]]       && die "--bfb-release is required"
[[ -z "$DOCA_HOST_URL" ]]     && die "--doca-host-url is required"
[[ -z "$RSHIM_URL" ]]         && die "--rshim-url is required"
[[ -z "$LIBFUSE2_URL" ]]      && die "--libfuse2-url is required"
[[ -z "$DOCA_HBN_VERSION" ]] && DOCA_HBN_VERSION="$DOCA_VERSION"
[[ -z "$HBN_CONFIG_URL" ]] && \
  HBN_CONFIG_URL="https://api.ngc.nvidia.com/v2/resources/org/nvidia/team/doca/doca_hbn/${DOCA_HBN_VERSION}/files"

BFB_NAME="bf-bundle-${DOCA_VERSION}-${BFB_BUILD}_${BFB_RELEASE}_ubuntu-22.04_prod.bfb"
DOCA_HBN_IMAGE="nvcr.io/nvidia/doca/doca_hbn:${HBN_CONTAINER_TAG}"
CONFIGS_DIR="doca_container_configs"
ZIP_OUTPUT="doca_container_configs.zip"

# ---------------------------------------------------------------------------
# Preflight checks
# ---------------------------------------------------------------------------
step "Preflight checks"

log "Checking required tools..."
for tool in wget docker curl jq base64 xxd zip gzip; do
  command -v "$tool" &>/dev/null && log "  $tool ... found" || die "$tool is not installed or not in PATH"
done
# sha256: prefer sha256sum (Linux/GNU), fall back to shasum (macOS)
if command -v sha256sum &>/dev/null && sha256sum --version &>/dev/null 2>&1; then
  log "  sha256sum ... found (GNU)"
elif command -v shasum &>/dev/null; then
  log "  shasum -a 256 ... found (macOS)"
else
  die "Neither sha256sum nor shasum found — install one and retry"
fi

log "Checking Docker daemon..."
if ! docker info &>/dev/null; then
  die "Docker daemon is not running. Start Docker and retry."
fi
ok "Docker daemon is running ($(docker version --format '{{.Server.Version}}' 2>/dev/null || echo 'version unknown'))"

log "Output directory: $(pwd)"
log "Disk space available: $(df -h . | awk 'NR==2{print $4}') free"

cat <<EOF

Configuration
  DOCA_VERSION:      ${DOCA_VERSION}
  DOCA_HBN_VERSION:  ${DOCA_HBN_VERSION}
  BFB_BUILD:         ${BFB_BUILD}
  BFB_RELEASE:       ${BFB_RELEASE}
  BFB_NAME:          ${BFB_NAME}
  BFB_URL:           ${BFB_BASE_URL}/${BFB_NAME}
  DOCA_HBN_IMAGE:    ${DOCA_HBN_IMAGE}
  HBN_CONFIG_URL:    ${HBN_CONFIG_URL}
  DOCA_HOST_URL:     ${DOCA_HOST_URL}
  RSHIM_URL:         ${RSHIM_URL}
  LIBFUSE2_URL:      ${LIBFUSE2_URL}
  Output dir:        $(pwd)
EOF

# ===========================================================================
# 1. BFB download (bfb-download)
# ===========================================================================
step "1/6 Downloading BFB"
log "URL: ${BFB_BASE_URL}/${BFB_NAME}"
wget -O "${BFB_NAME}" "${BFB_BASE_URL}/${BFB_NAME}" || die "BFB download failed — verify --bfb-build and --bfb-release are correct"
ok "Downloaded: ${BFB_NAME} ($(du -h "${BFB_NAME}" | cut -f1))"

log "Compressing ${BFB_NAME}..."
gzip -f "${BFB_NAME}"
ok "Compressed: ${BFB_NAME}.gz ($(du -h "${BFB_NAME}.gz" | cut -f1))"

# ===========================================================================
# 2. HBN container image → doca_hbn.tar.gz (bfb-hbn-pull, bfb-hbn-export)
# ===========================================================================
step "2/6 Building HBN container tarball"

log "Pulling image: ${DOCA_HBN_IMAGE} (platform: linux/arm64)"
docker pull --platform=linux/arm64 "${DOCA_HBN_IMAGE}"
ok "Image pulled: ${DOCA_HBN_IMAGE}"

log "Saving image to doca_hbn.tar..."
docker save --output="doca_hbn.tar" "${DOCA_HBN_IMAGE}"
ok "Saved: doca_hbn.tar ($(du -h doca_hbn.tar | cut -f1))"

log "Compressing doca_hbn.tar..."
gzip -f "doca_hbn.tar"
ok "Compressed: doca_hbn.tar.gz ($(du -h doca_hbn.tar.gz | cut -f1))"

# ===========================================================================
# 3. HBN configs + scripts → doca_container_configs.zip.gz
#    (mkdir-hbn-folder-in-bfb, download-hbn-installer-to-bfb,
#     mv-hbn-configs-to-bfb, mv-hbn-scripts-to-bfb, cleanup-hbn-scripts)
# ===========================================================================
step "3/6 Building HBN configs bundle"
{
  # Remove stale staging artifacts from any prior partial run so mkdir/mv
  # always start from a clean slate and never nest into leftover directories.
  rm -rf "${CONFIGS_DIR}"
  rm -f "${ZIP_OUTPUT}" "${ZIP_OUTPUT}.gz"

  # Clean up the staging dir on failure so a rerun starts fresh.
  trap 'rm -rf "${CONFIGS_DIR:-}" "${TEMP_DL:-}" 2>/dev/null; trap - EXIT' EXIT

  log "Creating staging directory: ${CONFIGS_DIR}/"
  mkdir -p "${CONFIGS_DIR}"
  TEMP_DL="${CONFIGS_DIR}/temp"
  mkdir -p "${TEMP_DL}"

  log "Fetching NGC file list from: ${HBN_CONFIG_URL}"
  files=$(curl -sSf "${HBN_CONFIG_URL}")
  file_count=$(printf '%s\n' "$files" | jq '.urls | length')
  log "Found ${file_count} files to download"

  n=0
  printf '%s\n' "$files" |
    jq -c '
      .urls as $u
    | .filepath as $p
    | .sha256_base64 as $s
    | range(0; $u | length) as $i
    | {url: $u[$i], filepath: $p[$i], sha256_base64: $s[$i]}
    ' |
    while IFS= read -r obj; do
      url=$(printf '%s\n'  "$obj" | jq -r '.url')
      path=$(printf '%s\n' "$obj" | jq -r '.filepath')
      sha256_b64=$(printf '%s\n' "$obj" | jq -r '.sha256_base64')
      dest="${TEMP_DL}/${path}"
      mkdir -p "$(dirname "$dest")"
      n=$((n + 1))
      log "  [${n}/${file_count}] ${path}"
      curl -sSL "${url}" -o "${dest}"
      if [[ "$sha256_b64" == "null" || -z "$sha256_b64" ]]; then
        log "  WARNING: no SHA256 provided by NGC for ${path} — skipping verification"
      else
        expected_sha=$(printf '%s\n' "$sha256_b64" | b64decode | xxd -p | tr -d '\n')
        actual_sha=$(sha256_of "${dest}")
        [[ "$actual_sha" == "$expected_sha" ]] \
          || die "SHA256 mismatch for ${path} (expected: ${expected_sha}, got: ${actual_sha})"
      fi
    done
  ok "All ${file_count} files downloaded and verified"

  log "Arranging scripts/${DOCA_HBN_VERSION}/..."
  if [[ -d "${TEMP_DL}/scripts/${DOCA_HBN_VERSION}" ]]; then
    mv "${TEMP_DL}/scripts/${DOCA_HBN_VERSION}" "${CONFIGS_DIR}/scripts"
    ok "scripts/ arranged"
  else
    log "WARNING: scripts/${DOCA_HBN_VERSION}/ not found — skipping"
  fi

  log "Arranging configs/${DOCA_HBN_VERSION}/..."
  if [[ -d "${TEMP_DL}/configs/${DOCA_HBN_VERSION}" ]]; then
    mv "${TEMP_DL}/configs/${DOCA_HBN_VERSION}" "${CONFIGS_DIR}/configs"
    ok "configs/ arranged"
  else
    log "WARNING: configs/${DOCA_HBN_VERSION}/ not found — skipping"
  fi

  log "Cleaning up temp download directory..."
  rm -rf "${TEMP_DL}"

  log "Packaging ${ZIP_OUTPUT}..."
  zip -qr "${ZIP_OUTPUT}" "${CONFIGS_DIR}/"
  rm -rf "${CONFIGS_DIR}"
  ok "Packaged: ${ZIP_OUTPUT} ($(du -h "${ZIP_OUTPUT}" | cut -f1))"

  log "Compressing ${ZIP_OUTPUT}..."
  gzip -f "${ZIP_OUTPUT}"
  ok "Compressed: ${ZIP_OUTPUT}.gz ($(du -h "${ZIP_OUTPUT}.gz" | cut -f1))"

  log "Writing doca_hbn_versions.cfg..."
  cat > "doca_hbn_versions.cfg" <<EOF
HBN_SCRIPT_DIR="./${CONFIGS_DIR}/scripts"
HBN_CONFIG_SRC_DIR="./${CONFIGS_DIR}/configs"
EOF
  ok "Written: doca_hbn_versions.cfg"
  trap - EXIT
}

# ===========================================================================
# 4. DOCA host package (.deb)
# ===========================================================================
DOCA_HOST_DEB="$(basename "$DOCA_HOST_URL")"

step "4/6 Downloading DOCA host package"
log "URL: ${DOCA_HOST_URL}"
wget -O "${DOCA_HOST_DEB}" "${DOCA_HOST_URL}" || die "DOCA host package download failed — verify --doca-host-url is correct"
ok "Downloaded: ${DOCA_HOST_DEB} ($(du -h "${DOCA_HOST_DEB}" | cut -f1))"

# ===========================================================================
# 5. rshim dependencies + rshim package (.deb)
# ===========================================================================
LIBFUSE2_DEB="$(basename "$LIBFUSE2_URL")"
RSHIM_DEB="$(basename "$RSHIM_URL")"

step "5/6 Downloading libfuse2t64 package (rshim dependency)"
log "URL: ${LIBFUSE2_URL}"
wget -O "${LIBFUSE2_DEB}" "${LIBFUSE2_URL}" || die "libfuse2t64 package download failed — verify --libfuse2-url is correct"
ok "Downloaded: ${LIBFUSE2_DEB} ($(du -h "${LIBFUSE2_DEB}" | cut -f1))"

# ===========================================================================
# 6. rshim package (.deb)
# ===========================================================================
step "6/6 Downloading rshim package"
log "URL: ${RSHIM_URL}"
wget -O "${RSHIM_DEB}" "${RSHIM_URL}" || die "rshim package download failed — verify --rshim-url is correct"
ok "Downloaded: ${RSHIM_DEB} ($(du -h "${RSHIM_DEB}" | cut -f1))"

# ===========================================================================
# Version config — always written so build-dpu-install-iso.sh can read
# DOCA_VERSION and HBN_VERSION from the artifacts dir when using --artifacts-dir
# ===========================================================================
log "Writing dpu_fw_version.cfg..."
cat > "dpu_fw_version.cfg" <<EOF
DOCA_VERSION="${DOCA_VERSION}"
HBN_VERSION="${DOCA_HBN_VERSION}"
HBN_CONTAINER_TAG="${HBN_CONTAINER_TAG}"
EOF
ok "Written: dpu_fw_version.cfg"

# ===========================================================================
# Summary
# ===========================================================================
echo
echo "============================================================"
echo "  Build complete — artifacts in $(pwd):"
echo "============================================================"
echo "  $(du -h "${BFB_NAME}.gz")  ${BFB_NAME}.gz"
echo "  $(du -h "doca_hbn.tar.gz")  doca_hbn.tar.gz"
echo "  $(du -h "${ZIP_OUTPUT}.gz")  ${ZIP_OUTPUT}.gz"
echo "  doca_hbn_versions.cfg"
echo "  dpu_fw_version.cfg"
echo "  $(du -h "${DOCA_HOST_DEB}")  ${DOCA_HOST_DEB}"
echo "  $(du -h "${LIBFUSE2_DEB}")  ${LIBFUSE2_DEB}"
echo "  $(du -h "${RSHIM_DEB}")  ${RSHIM_DEB}"
echo "============================================================"
