#!/usr/bin/env bash
# build-dpu-upgrade-iso.sh
# Builds a DPU firmware upgrade ISO and ZIP in a single run.
#
# Unlike build-dpu-install-iso.sh, the upgrade ISO carries no site config and
# no per-node servers/ directory: the upgrade flow preserves the DPU's live
# startup.yaml and the host's existing netplan, so the ISO is generic — the
# same ISO upgrades any BlueField DPU running the HBN stack.
#
# Usage:
#   ./build-dpu-upgrade-iso.sh --ubuntu-password-hash '<hash>' \
#                              --download-artifacts --doca-version 3.2.2 \
#                              --hbn-version 3.2.2 --bfb-build 125 ...
#
# Required (always):
#   --ubuntu-password-hash HASH    SHA-512 password hash for the ubuntu account
#                                  on the upgraded DPU (flashing wipes the DPU
#                                  OS). Generate with: openssl passwd -6 '<pw>'
#
# Required (one of):
#   --download-artifacts           Download artifacts; also requires --doca-version,
#                                  --hbn-version, --bfb-build, --bfb-release,
#                                  --hbn-container-tag, --doca-host-url,
#                                  --rshim-url, --libfuse2-url
#   --artifacts-dir DIR            Use pre-downloaded artifacts; DOCA_VERSION and
#                                  HBN_VERSION are read from dpu_fw_version.cfg
#                                  inside the directory (written by
#                                  download-build-dpu-artifacts.sh)
#
# Required with --download-artifacts:
#   --doca-version VERSION         DOCA platform version (e.g. 3.2.2)
#   --hbn-version VERSION          HBN version (e.g. 3.2.2)
#   --hbn-container-tag TAG        Exact NGC container tag for the HBN image, e.g.
#                                  3.2.2-doca3.2.2 or 2.4.2-doca2.9.2-32
#                                  (check https://catalog.ngc.nvidia.com for the tag)
#   --bfb-build NUMBER             BFB build number (e.g. 125)
#   --bfb-release VERSION          BFB release string (e.g. 26.02)
#   --doca-host-url URL            Full URL to DOCA host .deb package
#   --rshim-url URL                Full URL to rshim .deb package (from GitHub releases)
#   --libfuse2-url URL             Full URL to libfuse2t64 .deb package (rshim dependency)
#
# Optional with --download-artifacts:
#   --bfb-url URL            Base CDN URL for BFB download
#                            (default: https://content.mellanox.com/BlueField/BFBs/Ubuntu22.04)
#   --hbn-config-url URL     Full NGC files API URL for HBN config bundle
#                            (default: https://api.ngc.nvidia.com/v2/resources/org/nvidia/team/doca/doca_hbn/<hbn-version>/files)
#
# Optional:
#   --include-startup-yaml PATH
#                            A saved HBN startup.yaml file, or a directory of
#                            them, to embed in the ISO under startup-configs/.
#                            For DPU replacement/recovery: the host has no
#                            network until HBN is deployed on the new DPU, so
#                            a previously saved config must travel inside the
#                            ISO. May be given multiple times. On the host,
#                            use it with:
#                            upgrade-dpu-fw.sh --startup-yaml-file
#                              /var/lib/dpu-upgrade/<ver>/startup-configs/<name>
#   --output-dir DIR         Output directory (default: ./output)
#   --help                   Show this help
#
# Dependencies: gomplate, gzip, zip, unzip,
#               mkisofs (Linux) or xorrisofs (macOS);
#               --download-artifacts additionally needs the dependencies of
#               download-build-dpu-artifacts.sh (wget, docker, curl, jq, ...)
#
# Output (all under --output-dir):
#   dpu_upgrade_<ver>.iso        DPU upgrade ISO
#   dpu_upgrade_<ver>.zip        DPU upgrade ZIP

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARENT_DIR="$(dirname "$SCRIPT_DIR")"
ON_SERVER_DIR="$PARENT_DIR/on-server"
TEMPLATES_DIR="$ON_SERVER_DIR/templates"
UPGRADE_ON_SERVER_DIR="$SCRIPT_DIR/on-server"

# Print an error and abort the build.
die()  { echo "ERROR: $*" >&2; exit 1; }
# Timestamped progress line.
log()  { echo "[$(date '+%H:%M:%S')] $*"; }
# Banner for a major build phase.
step() { echo; echo "------------------------------------------------------------"; \
         echo "[$(date '+%H:%M:%S')] $*"; \
         echo "------------------------------------------------------------"; }

# Print this script's header comment as usage text and exit.
usage() {
    grep '^#' "$0" | grep -v '#!/' | sed 's/^# \{0,1\}//'
    exit 1
}

# ── Argument parsing ──────────────────────────────────────────────────────────

UBUNTU_PASSWORD_HASH=""
DOCA_VERSION=""
HBN_VERSION=""
HBN_CONTAINER_TAG=""
BFB_BUILD=""
BFB_RELEASE=""
BFB_URL=""
HBN_CONFIG_URL=""
DOCA_HOST_URL=""
RSHIM_URL=""
LIBFUSE2_URL=""
OUTPUT_DIR=""
ARTIFACTS_DIR=""
DOWNLOAD_ARTIFACTS=false
INCLUDE_STARTUP_YAMLS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ubuntu-password-hash) [[ -z "${2:-}" ]] && die "$1 requires a value"; UBUNTU_PASSWORD_HASH="$2"; shift 2 ;;
        --doca-version)         [[ -z "${2:-}" ]] && die "$1 requires a value"; DOCA_VERSION="$2";         shift 2 ;;
        --hbn-version)          [[ -z "${2:-}" ]] && die "$1 requires a value"; HBN_VERSION="$2";          shift 2 ;;
        --hbn-container-tag)    [[ -z "${2:-}" ]] && die "$1 requires a value"; HBN_CONTAINER_TAG="$2";    shift 2 ;;
        --bfb-build)            [[ -z "${2:-}" ]] && die "$1 requires a value"; BFB_BUILD="$2";            shift 2 ;;
        --bfb-release)          [[ -z "${2:-}" ]] && die "$1 requires a value"; BFB_RELEASE="$2";          shift 2 ;;
        --bfb-url)              [[ -z "${2:-}" ]] && die "$1 requires a value"; BFB_URL="$2";              shift 2 ;;
        --hbn-config-url)       [[ -z "${2:-}" ]] && die "$1 requires a value"; HBN_CONFIG_URL="$2";       shift 2 ;;
        --doca-host-url)        [[ -z "${2:-}" ]] && die "$1 requires a value"; DOCA_HOST_URL="$2";        shift 2 ;;
        --rshim-url)            [[ -z "${2:-}" ]] && die "$1 requires a value"; RSHIM_URL="$2";            shift 2 ;;
        --libfuse2-url)         [[ -z "${2:-}" ]] && die "$1 requires a value"; LIBFUSE2_URL="$2";         shift 2 ;;
        --output-dir)           [[ -z "${2:-}" ]] && die "$1 requires a value"; OUTPUT_DIR="$2";           shift 2 ;;
        --artifacts-dir)        [[ -z "${2:-}" ]] && die "$1 requires a value"; ARTIFACTS_DIR="$2";        shift 2 ;;
        --include-startup-yaml) [[ -z "${2:-}" ]] && die "$1 requires a value"; INCLUDE_STARTUP_YAMLS+=("$2"); shift 2 ;;
        --download-artifacts)   DOWNLOAD_ARTIFACTS=true;       shift ;;
        --help|-h) usage ;;
        *) die "Unknown option: $1" ;;
    esac
done

# Count guard keeps the empty-array expansion safe on older bash (macOS 3.2)
if [[ "${#INCLUDE_STARTUP_YAMLS[@]}" -gt 0 ]]; then
    for p in "${INCLUDE_STARTUP_YAMLS[@]}"; do
        [[ -e "$p" ]] || die "--include-startup-yaml path not found: $p"
    done
fi

[[ -z "$UBUNTU_PASSWORD_HASH" ]] && die "--ubuntu-password-hash is required (generate with: openssl passwd -6 '<password>')"
[[ "$UBUNTU_PASSWORD_HASH" == \$* ]] \
    || die "--ubuntu-password-hash does not look like a crypt hash (expected to start with '\$', e.g. from: openssl passwd -6 '<password>')"

[[ -z "$OUTPUT_DIR" ]] && OUTPUT_DIR="./output"
if [[ -d "$OUTPUT_DIR" ]] && [[ -n "$(ls -A "$OUTPUT_DIR" 2>/dev/null)" ]]; then
    die "Output directory $OUTPUT_DIR already exists and is non-empty. Remove it manually or choose a different --output-dir."
fi
OUTPUT_DIR="$(mkdir -p "$OUTPUT_DIR" && cd "$OUTPUT_DIR" && pwd)"

if [[ -z "$ARTIFACTS_DIR" ]] && [[ "$DOWNLOAD_ARTIFACTS" == false ]]; then
    die "either --download-artifacts or --artifacts-dir is required"
fi

if [[ "$DOWNLOAD_ARTIFACTS" == true ]]; then
    [[ -z "$DOCA_VERSION" ]]      && die "--doca-version is required with --download-artifacts"
    [[ -z "$HBN_VERSION" ]]       && die "--hbn-version is required with --download-artifacts"
    [[ -z "$HBN_CONTAINER_TAG" ]] && die "--hbn-container-tag is required with --download-artifacts"
    [[ -z "$BFB_BUILD" ]]         && die "--bfb-build is required with --download-artifacts"
    [[ -z "$BFB_RELEASE" ]]       && die "--bfb-release is required with --download-artifacts"
    [[ -z "$DOCA_HOST_URL" ]]     && die "--doca-host-url is required with --download-artifacts"
    [[ -z "$RSHIM_URL" ]]         && die "--rshim-url is required with --download-artifacts"
    [[ -z "$LIBFUSE2_URL" ]]      && die "--libfuse2-url is required with --download-artifacts"
else
    [[ -d "$ARTIFACTS_DIR" ]] || die "--artifacts-dir not found: $ARTIFACTS_DIR"
    _ver_cfg="$ARTIFACTS_DIR/dpu_fw_version.cfg"
    [[ -f "$_ver_cfg" ]] || die "dpu_fw_version.cfg not found in $ARTIFACTS_DIR — was it created by download-build-dpu-artifacts.sh?"
    # shellcheck source=/dev/null
    source "$_ver_cfg"
    [[ -z "${DOCA_VERSION:-}" ]] && die "DOCA_VERSION not set in $_ver_cfg"
    [[ -z "${HBN_VERSION:-}" ]]  && die "HBN_VERSION not set in $_ver_cfg"
    log "Versions from artifacts dir: DOCA=${DOCA_VERSION} HBN=${HBN_VERSION}"
fi

ISO_BASE="dpu_upgrade_${DOCA_VERSION}_${HBN_VERSION}"
ISO_OUT="$OUTPUT_DIR/${ISO_BASE}.iso"
ZIP_OUT="$OUTPUT_DIR/${ISO_BASE}.zip"

# ── Preflight ─────────────────────────────────────────────────────────────────

step "Preflight checks"

for f in \
    "$PARENT_DIR/download-build-dpu-artifacts.sh" \
    "$ON_SERVER_DIR/dpuinstall.sh" \
    "$UPGRADE_ON_SERVER_DIR/upgrade-install.sh" \
    "$UPGRADE_ON_SERVER_DIR/upgrade-dpu-fw.sh" \
    "$UPGRADE_ON_SERVER_DIR/upgrade-post-power-cycle.sh" \
    "$UPGRADE_ON_SERVER_DIR/upgrade-lib.sh" \
    "$TEMPLATES_DIR/bf.cfg.template"; do
    [[ -f "$f" ]] || die "Required file not found: $f"
done
log "Script and template files: OK"

command -v gomplate &>/dev/null || die "gomplate is required — https://docs.gomplate.ca/installing/"
command -v zip      &>/dev/null || die "zip is required"
command -v unzip    &>/dev/null || die "unzip is required"
command -v gunzip   &>/dev/null || die "gunzip is required"

if [[ "$(uname)" == "Darwin" ]]; then
    ISO_TOOL="xorrisofs"
else
    ISO_TOOL="mkisofs"
fi
command -v "$ISO_TOOL" &>/dev/null || die "$ISO_TOOL not found — install genisoimage (Linux) or xorriso (macOS)"
log "Tools: OK (ISO tool: $ISO_TOOL)"

# ── bf.cfg renderer (gomplate) ────────────────────────────────────────────────

# Render bf.cfg.template with the operator-supplied ubuntu password hash.
render_bf_cfg() {
    local vars_file
    vars_file="$(mktemp "$OUTPUT_DIR/gomplate_vars_XXXXXX")"
    printf 'UbuntuPasswordHash: "%s"\n' "$UBUNTU_PASSWORD_HASH" > "$vars_file"
    gomplate --file "$TEMPLATES_DIR/bf.cfg.template" --context ".=${vars_file}?type=application/yaml"
    local rc=$?
    rm -f "$vars_file"
    return $rc
}

# ── Step 1: Download artifacts ───────────────────────────────────────────────

CLEANUP_ARTIFACTS_DIR=""

if [[ "$DOWNLOAD_ARTIFACTS" == false ]]; then
    step "Using pre-built artifacts from $ARTIFACTS_DIR"
else
    step "Downloading artifacts"
    ARTIFACTS_DIR="$(mktemp -d)"
    CLEANUP_ARTIFACTS_DIR="$ARTIFACTS_DIR"
    trap 'rm -rf ${STAGE_DIR:-} ${CLEANUP_ARTIFACTS_DIR:-}' EXIT

    BUILD_ARGS=(
        --doca-version "$DOCA_VERSION"
        --bfb-build    "$BFB_BUILD"
        --bfb-release  "$BFB_RELEASE"
    )
    [[ "$HBN_VERSION" != "$DOCA_VERSION" ]] && BUILD_ARGS+=(--doca-hbn-version "$HBN_VERSION")
    BUILD_ARGS+=(--hbn-container-tag "$HBN_CONTAINER_TAG")
    [[ -n "$BFB_URL" ]]        && BUILD_ARGS+=(--bfb-url "$BFB_URL")
    [[ -n "$HBN_CONFIG_URL" ]] && BUILD_ARGS+=(--hbn-config-url "$HBN_CONFIG_URL")
    BUILD_ARGS+=(--doca-host-url "$DOCA_HOST_URL")
    BUILD_ARGS+=(--rshim-url     "$RSHIM_URL")
    BUILD_ARGS+=(--libfuse2-url  "$LIBFUSE2_URL")

    log "Running download-build-dpu-artifacts.sh — output dir: $ARTIFACTS_DIR"
    (cd "$ARTIFACTS_DIR" && bash "$PARENT_DIR/download-build-dpu-artifacts.sh" "${BUILD_ARGS[@]}")
fi

# ── Step 1b: Validate HBN config bundle ──────────────────────────────────────

step "Validating HBN config bundle"

_hbn_ver_cfg="$ARTIFACTS_DIR/doca_hbn_versions.cfg"
[[ -f "$_hbn_ver_cfg" ]] || die "doca_hbn_versions.cfg not found in $ARTIFACTS_DIR"

# shellcheck source=/dev/null
source "$_hbn_ver_cfg"
[[ -z "${HBN_SCRIPT_DIR:-}" ]]     && die "HBN_SCRIPT_DIR not set in doca_hbn_versions.cfg"
[[ -z "${HBN_CONFIG_SRC_DIR:-}" ]] && die "HBN_CONFIG_SRC_DIR not set in doca_hbn_versions.cfg"
log "HBN_SCRIPT_DIR:    $HBN_SCRIPT_DIR"
log "HBN_CONFIG_SRC_DIR: $HBN_CONFIG_SRC_DIR"

# Strip leading ./ for zip path matching
_script_dir="${HBN_SCRIPT_DIR#./}"
_config_dir="${HBN_CONFIG_SRC_DIR#./}"

_zip=""
for f in "$ARTIFACTS_DIR"/doca_container_configs.zip "$ARTIFACTS_DIR"/doca_container_configs.zip.gz; do
    [[ -f "$f" ]] && _zip="$f" && break
done
[[ -z "$_zip" ]] && die "doca_container_configs.zip(.gz) not found in $ARTIFACTS_DIR"

if [[ "$_zip" == *.gz ]]; then
    _tmp_zip=$(mktemp /tmp/hbn_cfg_XXXXXX)
    gunzip -c "$_zip" > "$_tmp_zip"
    _zip_list=$(unzip -l "$_tmp_zip" | awk '{print $4}')
    rm -f "$_tmp_zip"
else
    _zip_list=$(unzip -l "$_zip" | awk '{print $4}')
fi

# Check HBN_SCRIPT_DIR exists in zip
if ! echo "$_zip_list" | grep -q "^${_script_dir}/"; then
    die "HBN_SCRIPT_DIR '${_script_dir}/' not found in doca_container_configs.zip — check doca_hbn_versions.cfg"
fi
log "HBN_SCRIPT_DIR found in zip: OK"

# Check hbn-dpu-setup.sh exists in HBN_SCRIPT_DIR
if ! echo "$_zip_list" | grep -q "^${_script_dir}/hbn-dpu-setup.sh$"; then
    die "hbn-dpu-setup.sh not found at '${_script_dir}/hbn-dpu-setup.sh' in zip"
fi
log "hbn-dpu-setup.sh found in zip: OK"

# Check HBN_CONFIG_SRC_DIR exists in zip
if ! echo "$_zip_list" | grep -q "^${_config_dir}/"; then
    die "HBN_CONFIG_SRC_DIR '${_config_dir}/' not found in doca_container_configs.zip — check doca_hbn_versions.cfg"
fi
log "HBN_CONFIG_SRC_DIR found in zip: OK"

# Check doca_hbn.yaml exists in HBN_CONFIG_SRC_DIR
if ! echo "$_zip_list" | grep -q "^${_config_dir}/doca_hbn.yaml$"; then
    die "doca_hbn.yaml not found at '${_config_dir}/doca_hbn.yaml' in zip — was it included in the artifact download?"
fi
log "doca_hbn.yaml found in zip: OK"

# ── Step 2: Assemble ISO staging directory ───────────────────────────────────

step "Assembling ISO contents"

STAGE_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGE_DIR" ${CLEANUP_ARTIFACTS_DIR:-}' EXIT

cp "$UPGRADE_ON_SERVER_DIR/upgrade-install.sh"           "$STAGE_DIR/"
cp "$UPGRADE_ON_SERVER_DIR/upgrade-dpu-fw.sh"            "$STAGE_DIR/"
cp "$UPGRADE_ON_SERVER_DIR/upgrade-post-power-cycle.sh"  "$STAGE_DIR/"
cp "$UPGRADE_ON_SERVER_DIR/upgrade-lib.sh"               "$STAGE_DIR/"
render_bf_cfg > "$STAGE_DIR/bf.cfg.template"
chmod 755 "$STAGE_DIR/upgrade-install.sh" \
          "$STAGE_DIR/upgrade-dpu-fw.sh" \
          "$STAGE_DIR/upgrade-post-power-cycle.sh"

# dpuinstall.sh is taken verbatim from the install toolchain — the upgrade
# flow reuses its functions unchanged.
cp "$ON_SERVER_DIR/dpuinstall.sh" "$STAGE_DIR/"
chmod 755 "$STAGE_DIR/dpuinstall.sh"

cat > "$STAGE_DIR/dpu_fw_version.cfg" <<EOF
DOCA_VERSION="${DOCA_VERSION}"
HBN_VERSION="${HBN_VERSION}"
EOF

cat > "$STAGE_DIR/doca_hbn_versions.cfg" <<EOF
HBN_SCRIPT_DIR="${HBN_SCRIPT_DIR}"
HBN_CONFIG_SRC_DIR="${HBN_CONFIG_SRC_DIR}"
EOF

# Embedded startup configs for DPU replacement / recovery — the host has no
# network with a blank DPU, so these must travel inside the ISO.
if [[ "${#INCLUDE_STARTUP_YAMLS[@]}" -gt 0 ]]; then
    log "Embedding startup configs under startup-configs/..."
    mkdir -p "$STAGE_DIR/startup-configs"
    # Copy one saved startup.yaml into the ISO staging dir, rejecting empties and duplicates.
    stage_startup_config() {
        local f="$1" dest
        dest="$STAGE_DIR/startup-configs/$(basename "$f")"
        [[ -s "$f" ]] || die "--include-startup-yaml: $f is empty"
        [[ -e "$dest" ]] && die "--include-startup-yaml: duplicate config name: $(basename "$f")"
        cp "$f" "$dest"
        log "  startup-configs/$(basename "$f")"
    }
    for p in "${INCLUDE_STARTUP_YAMLS[@]}"; do
        if [[ -d "$p" ]]; then
            _found=false
            for f in "$p"/*; do
                [[ -f "$f" ]] || continue
                stage_startup_config "$f"
                _found=true
            done
            [[ "$_found" == true ]] || die "--include-startup-yaml: no files found in directory: $p"
        else
            stage_startup_config "$p"
        fi
    done
fi

log "Copying artifacts..."
copied=0
for pattern in "*.bfb" "*.bfb.gz" "*.deb" "*.deb.gz" "*.tar" "*.tar.gz" "*.zip" "*.zip.gz"; do
    for f in "$ARTIFACTS_DIR"/$pattern; do
        [[ -e "$f" ]] || continue
        cp "$f" "$STAGE_DIR/"
        log "  $(basename "$f")  ($(du -h "$f" | cut -f1))"
        copied=$(( copied + 1 ))
    done
done
[[ "$copied" -eq 0 ]] && die "No artifacts found in $ARTIFACTS_DIR — download may have failed"

log "Staging layout:"
find "$STAGE_DIR" -type f | sort | sed "s|$STAGE_DIR/||" | while read -r f; do
    log "  $f"
done

# ── Step 3: Build ISO ─────────────────────────────────────────────────────────

step "Building ISO: $(basename "$ISO_OUT")"
"$ISO_TOOL" -quiet -J -r -V "dpu-upgrade" -o "$ISO_OUT" "$STAGE_DIR"
log "Created: $ISO_OUT  ($(du -h "$ISO_OUT" | cut -f1))"

# ── Step 4: Build ZIP ─────────────────────────────────────────────────────────

step "Building ZIP: $(basename "$ZIP_OUT")"
(cd "$STAGE_DIR" && zip -qr "$ZIP_OUT" .)
log "Created: $ZIP_OUT  ($(du -h "$ZIP_OUT" | cut -f1))"

# ── Summary ───────────────────────────────────────────────────────────────────

echo
echo "============================================================"
echo "  Done. Output: $OUTPUT_DIR"
echo "============================================================"
echo "  $(du -h "$ISO_OUT")"
echo "  $(du -h "$ZIP_OUT")"
echo "============================================================"
echo
echo "To upgrade a DPU:"
echo "  mount -o ro,loop $(basename "$ISO_OUT") /mnt/dpu-upgrade"
echo "  /mnt/dpu-upgrade/upgrade-install.sh"
echo "  /var/lib/dpu-upgrade/${DOCA_VERSION}_${HBN_VERSION}/upgrade-dpu-fw.sh --ssh-key <path>   # or: --auth password"
echo "  (power cycle)"
echo "  /var/lib/dpu-upgrade/${DOCA_VERSION}_${HBN_VERSION}/upgrade-post-power-cycle.sh"
