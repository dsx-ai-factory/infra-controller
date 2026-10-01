#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# =============================================================================
# preflight.sh — pre-flight checks for setup.sh
#
# Run standalone before setup.sh to catch configuration issues early:
#   source ./preflight.sh
#
# Also sourced automatically at the start of every setup.sh run.
#
# Checks (in order — fails fast so the most actionable issues appear first):
#   1. Environment variables    — presence and format
#   2. Required tools           — helm, helmfile, kubectl, jq, ssh-keygen
#   3. values/metallb-config.yaml — YAML, pools, advertisement mode, ASNs
#   4. Cluster reachability     — kubectl can reach the API server
#   5. Node resources           — at least 3 schedulable (Ready + untainted) nodes
#   6. MetalLB BGPPeer nodes    — hostnames in config exist in the cluster
#   7. Per-node checks          — kernel params (sysctl) and DNS on every node
#   8. Registry connectivity    — registry host is reachable over HTTPS
#   9. NICo REST source/charts   — in-tree rest-api/ and helm/rest/ are present
#
# Configurable:
#   NICO_CORE_IMAGE_NAME  — NICo Core image name under NICO_IMAGE_REGISTRY
#                           (default: nico; use nvmetal-carbide for legacy images)
#   PREFLIGHT_CHECK_IMAGE — image used for per-node pod checks (default: busybox:1.36)
#                           Override for air-gapped clusters:
#                           export PREFLIGHT_CHECK_IMAGE=my-registry.example.com/busybox:1.36
#
# Exit codes:
#   0 — all checks passed (or user chose to continue despite issues)
#   1 — hard failure or user declined to continue
# =============================================================================

# ---------------------------------------------------------------------------
# 0. Shell compatibility — must run under bash 3.2+ (macOS ships 3.2).
#    Catches `sh preflight.sh` / dash / ancient bash before cryptic errors.
# ---------------------------------------------------------------------------
if [ -z "${BASH_VERSION:-}" ]; then
    echo "ERROR: this script must be run under bash (not sh/dash/zsh)." >&2
    echo "  Try: bash ./setup.sh   (or source it from a bash shell)" >&2
    exit 1
fi
if [ "${BASH_VERSINFO[0]}" -lt 3 ] || \
   { [ "${BASH_VERSINFO[0]}" -eq 3 ] && [ "${BASH_VERSINFO[1]}" -lt 2 ]; }; then
    echo "ERROR: bash 3.2+ required (you have ${BASH_VERSION})." >&2
    echo "  On macOS: /bin/bash is 3.2 and works. If you're on something older," >&2
    echo "  install a newer bash: brew install bash" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Detect whether we are being sourced or executed directly.
# return in a function always returns from the function, not the script —
# so we use _SOURCED inline at every exit point instead.
_SOURCED=false
[[ "${BASH_SOURCE[0]}" != "${0}" ]] && _SOURCED=true

# ---------------------------------------------------------------------------
# Standalone flags — when run directly (`./preflight.sh --skip-rest`) accept the
# same skip flags as setup.sh so the checks match the install you intend to run.
# When SOURCED by setup.sh these already arrive as exported SKIP_* env vars and
# setup.sh has consumed its own args, so this block is a no-op there.
# ---------------------------------------------------------------------------
if ! ${_SOURCED}; then
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --skip-core)      SKIP_CORE=true ;;
            --skip-rest)      SKIP_REST=true ;;
            --skip-flow)      SKIP_FLOW=true ;;
            -y|--yes)         AUTO_YES=true ;;
            --core-values)    CORE_VALUES="$2"; shift ;;
            --metallb-config) METALLB_CONFIG="$2"; shift ;;
            *) ;;  # ignore unknown args when run standalone
        esac
        shift
    done
fi

ERRORS=()
WARNINGS=()
NICO_CORE_IMAGE_NAME="${NICO_CORE_IMAGE_NAME:-nico}"

_CORE_VALUES_CFG="${CORE_VALUES:-${SCRIPT_DIR}/values/nico-core.yaml}"
_CORE_VALUES_LABEL="${CORE_VALUES:-values/nico-core.yaml}"
_CORE_IMAGE_PULL_SECRETS=""

_collect_image_pull_secret_names() {
    awk '
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*imagePullSecrets:[[:space:]]*($|#)/ {
            in_block = 1
            block_indent = match($0, /[^ ]/) - 1
            next
        }
        in_block {
            if ($0 ~ /^[[:space:]]*($|#)/) next
            current_indent = match($0, /[^ ]/) - 1
            if (current_indent <= block_indent && $0 !~ /^[[:space:]]*-[[:space:]]*/) {
                in_block = 0
                next
            }
            if ($0 ~ /^[[:space:]]*-[[:space:]]*name:[[:space:]]*/) {
                name = $0
                sub(/^[[:space:]]*-[[:space:]]*name:[[:space:]]*/, "", name)
                sub(/[[:space:]]*#.*$/, "", name)
                gsub(/["\047]/, "", name)
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", name)
                if (length(name) > 0) print name
            }
        }
    ' "$1" | sort -u
}

if [[ "${SKIP_CORE:-false}" != "true" ]]; then
    if [[ -f "${_CORE_VALUES_CFG}" ]]; then
        _CORE_IMAGE_PULL_SECRETS="$(_collect_image_pull_secret_names "${_CORE_VALUES_CFG}")"
    else
        ERRORS+=("${_CORE_VALUES_LABEL} not found — pass --core-values <file> or restore helm-prereqs/values/nico-core.yaml")
    fi
fi

# ---------------------------------------------------------------------------
# Cleanup: remove any temp pods created by per-node checks
# ---------------------------------------------------------------------------
_PREFLIGHT_PODS=()
_PREFLIGHT_NS="kube-system"

_cleanup_preflight_pods() {
    [[ ${#_PREFLIGHT_PODS[@]} -eq 0 ]] && return
    kubectl delete pod "${_PREFLIGHT_PODS[@]}" \
        -n "${_PREFLIGHT_NS}" --ignore-not-found --wait=false >/dev/null 2>&1 || true
}

# ---------------------------------------------------------------------------
# 1. Environment variables — presence
# ---------------------------------------------------------------------------
if [[ -z "${REGISTRY_PULL_SECRET:-}" ]]; then
    if [[ "${SKIP_CORE:-false}" == "true" && "${SKIP_REST:-false}" == "true" ]]; then
        WARNINGS+=("REGISTRY_PULL_SECRET is not set  (imagepullsecret creation will be skipped)")
    else
        WARNINGS+=("REGISTRY_PULL_SECRET is not set  (setup.sh will not create imagepullsecret; images must be public, preloaded, or use existing imagePullSecrets)")
    fi
fi

if [[ "${SKIP_CORE:-false}" != "true" || "${SKIP_REST:-false}" != "true" ]]; then
    [[ -z "${NICO_IMAGE_REGISTRY:-}" ]] && \
        ERRORS+=("NICO_IMAGE_REGISTRY is not set    (container registry, e.g. my-registry.example.com/nico)")
fi

[[ "${SKIP_CORE:-false}" != "true" && -z "${NICO_CORE_IMAGE_TAG:-}" ]] && \
    ERRORS+=("NICO_CORE_IMAGE_TAG is not set    (NICo Core image tag, e.g. v2025.12.30)")

[[ "${SKIP_REST:-false}" != "true" && -z "${NICO_REST_IMAGE_TAG:-}" ]] && \
    ERRORS+=("NICO_REST_IMAGE_TAG is not set    (NICo REST image tag, e.g. v1.0.4)")

# Environment variables — format validation
_UUID_RE='^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'

# NICO_IMAGE_REGISTRY must not include a protocol prefix
if [[ -n "${NICO_IMAGE_REGISTRY:-}" ]] && [[ "${NICO_IMAGE_REGISTRY}" =~ ^https?:// ]]; then
    ERRORS+=("NICO_IMAGE_REGISTRY must not include a protocol prefix — remove 'https://' or 'http://'")
fi

# NICO_CORE_IMAGE_NAME is appended to NICO_IMAGE_REGISTRY as one repository
# path component. Reject full paths and tags so the resulting reference is
# unambiguous and remains under the configured registry prefix.
if [[ "${SKIP_CORE:-false}" != "true" ]] && \
   [[ ! "${NICO_CORE_IMAGE_NAME}" =~ ^[a-z0-9][a-z0-9._-]*$ ]]; then
    ERRORS+=("NICO_CORE_IMAGE_NAME='${NICO_CORE_IMAGE_NAME}' is not a valid image name — use one lowercase path component without a tag (e.g. nico or nvmetal-carbide)")
fi

# NICO_SITE_UUID must be a valid UUID if set (used as Temporal namespace + CLUSTER_ID)
if [[ -n "${NICO_SITE_UUID:-}" ]]; then
    if [[ ! "${NICO_SITE_UUID}" =~ ${_UUID_RE} ]]; then
        ERRORS+=("NICO_SITE_UUID='${NICO_SITE_UUID}' is not a valid UUID — the site-agent will fatal on startup (generate one with: python3 -c 'import uuid; print(uuid.uuid4())')")
    fi
fi

# REGISTRY_PULL_SECRET should not be an obvious placeholder
if [[ -n "${REGISTRY_PULL_SECRET:-}" ]]; then
    if [[ "${REGISTRY_PULL_SECRET}" =~ ^(<|your|placeholder|changeme|xxx|TODO) ]]; then
        WARNINGS+=("REGISTRY_PULL_SECRET looks like a placeholder value — set it to your actual registry pull secret")
    fi
fi

# KUBECONFIG file must exist if explicitly set
if [[ -n "${KUBECONFIG:-}" && ! -f "${KUBECONFIG}" ]]; then
    ERRORS+=("KUBECONFIG='${KUBECONFIG}' does not exist — check the path to your cluster kubeconfig")
fi

# ---------------------------------------------------------------------------
# 2. Required tools
# ---------------------------------------------------------------------------
for _tool in helm helmfile kubectl jq ssh-keygen; do
    command -v "${_tool}" &>/dev/null || \
        WARNINGS+=("'${_tool}' not found in PATH — install it before running setup.sh")
done

# ---------------------------------------------------------------------------
# 3. values/metallb-config.yaml — static checks (no cluster access needed)
# ---------------------------------------------------------------------------
_METALLB_CFG="${METALLB_CONFIG:-${SCRIPT_DIR}/values/metallb-config.yaml}"
_METALLB_CFG_LABEL="${METALLB_CONFIG:-values/metallb-config.yaml}"
_METALLB_RENDERED=""

if [[ ! -e "${_METALLB_CFG}" ]]; then
    ERRORS+=("${_METALLB_CFG_LABEL} not found — restore from git or pass --metallb-config")
else
    # Render once, then validate the same effective manifest that setup.sh applies.
    if command -v kubectl &>/dev/null; then
        if [[ -d "${_METALLB_CFG}" ]]; then
            _METALLB_RENDERED="$(kubectl kustomize "${_METALLB_CFG}" 2>/dev/null)" || \
                ERRORS+=("${_METALLB_CFG_LABEL}: kustomize render failed")
        else
            _METALLB_RENDERED="$(cat "${_METALLB_CFG}")"
        fi
        # YAML syntax — kubectl dry-run with validate=false, but filter out
        # API discovery errors. MetalLB CRDs may not be installed yet, and
        # cluster reachability is checked separately below.
        if [[ -n "${_METALLB_RENDERED}" ]]; then
            _yaml_out="$(printf '%s\n' "${_METALLB_RENDERED}" | \
                kubectl apply --dry-run=client --validate=false -f - 2>&1)" || true
            _yaml_real_errors="$(echo "${_yaml_out}" | \
                grep -Ei 'error:|unable to|invalid|yaml|json|cannot|could not|failed|no matches for kind|resource mapping not found|ensure CRDs are installed|couldn.t get current server API group list|The connection to the server|dial tcp|i/o timeout|context deadline exceeded|no route to host|network is unreachable|connect: connection refused' | \
                grep -vE 'no matches for kind|resource mapping not found|ensure CRDs are installed|couldn.t get current server API group list|unable to recognize .*: Get |The connection to the server|Unable to connect to the server|dial tcp|i/o timeout|context deadline exceeded|no route to host|network is unreachable|connect: connection refused' || true)"
            if [[ -n "${_yaml_real_errors}" ]]; then
                ERRORS+=("${_METALLB_CFG_LABEL}: YAML parse error — ${_yaml_real_errors}")
            fi
        fi
    elif [[ -f "${_METALLB_CFG}" ]]; then
        _METALLB_RENDERED="$(cat "${_METALLB_CFG}")"
    else
        WARNINGS+=("${_METALLB_CFG_LABEL}: cannot render kustomize directory because kubectl is not available")
    fi

    # At least one active IPAddressPool
    if [[ -n "${_METALLB_RENDERED}" ]] && \
       ! printf '%s\n' "${_METALLB_RENDERED}" | grep -qE '^kind: IPAddressPool'; then
        ERRORS+=("${_METALLB_CFG_LABEL}: no IPAddressPool defined")
    fi

    # Advertisement mode consistency
    _n_bgp_peer=$(printf '%s\n' "${_METALLB_RENDERED}" | grep -cE '^kind: BGPPeer' || true)
    _n_bgp_adv=$( printf '%s\n' "${_METALLB_RENDERED}" | grep -cE '^kind: BGPAdvertisement' || true)
    _n_l2_adv=$(  printf '%s\n' "${_METALLB_RENDERED}" | grep -cE '^kind: L2Advertisement' || true)

    if [[ -n "${_METALLB_RENDERED}" ]]; then
        if [[ "${_n_bgp_peer}" -gt 0 && "${_n_l2_adv}" -gt 0 ]]; then
            ERRORS+=("${_METALLB_CFG_LABEL}: BGPPeer and L2Advertisement are both active — choose one mode only")
        elif [[ "${_n_bgp_peer}" -eq 0 && "${_n_l2_adv}" -eq 0 ]]; then
            ERRORS+=("${_METALLB_CFG_LABEL}: no advertisement mode configured — add BGPPeer+BGPAdvertisement (BGP) or L2Advertisement (L2)")
        elif [[ "${_n_bgp_peer}" -gt 0 && "${_n_bgp_adv}" -eq 0 ]]; then
            ERRORS+=("${_METALLB_CFG_LABEL}: BGPPeer defined but no BGPAdvertisement — VIPs will not be announced")
        fi
    fi

    # BGP ASNs must be non-zero integers
    while IFS= read -r _line; do
        if [[ "${_line}" =~ ^[[:space:]]*(my|peer)ASN:[[:space:]]*([0-9]+) ]]; then
            [[ "${BASH_REMATCH[2]}" -eq 0 ]] && \
                ERRORS+=("${_METALLB_CFG_LABEL}: ASN value is 0 — set a valid BGP ASN")
        fi
    done < <(printf '%s\n' "${_METALLB_RENDERED}")
fi

# ---------------------------------------------------------------------------
# 3b. Site values completeness — every site-specific value must be populated.
#     Fails on leftover EXAMPLE/placeholder tokens in ACTIVE (non-comment) lines of
#     values.yaml + values/nico-core.yaml + values/metallb-config.yaml, and on a few
#     required keys. (Comment lines and inline `# …` comments are stripped first.)
# ---------------------------------------------------------------------------
_SITE_VALUES_CFG="${SCRIPT_DIR}/values.yaml"
_PLACEHOLDER_RE='EXAMPLE|examplesite|example\.com|TMP_SITE|REPLACE_WITH|<your-registry>|<yoursite>|your-site|changeme|REPLACE_ME'
for _vf in "${_SITE_VALUES_CFG}" "${_CORE_VALUES_CFG}" "${_METALLB_CFG}"; do
    [[ -f "${_vf}" ]] || continue
    _hits="$(sed -E 's/[[:space:]]+#.*$//; /^[[:space:]]*#/d' "${_vf}" | grep -nEi "${_PLACEHOLDER_RE}" || true)"
    if [[ -n "${_hits}" ]]; then
        ERRORS+=("${_vf##*/}: unpopulated placeholder value(s) — every site value must be set:"$'\n'"$(printf '%s\n' "${_hits}" | sed 's/^/          /')")
    fi
done

# Empty-but-required site values — the committed templates ship BLANK (no site
# data). Every active (uncommented) required value must be filled before install.
# Comment lines + inline `# …` comments are stripped first.
_strip_comments() { sed -E 's/[[:space:]]+#.*$//; /^[[:space:]]*#/d' "$1"; }

if [[ "${SKIP_CORE:-false}" != "true" && -f "${_CORE_VALUES_CFG}" ]]; then
    # nico-api.hostname must be a real external hostname
    if _strip_comments "${_CORE_VALUES_CFG}" | grep -qE '^[[:space:]]*hostname:[[:space:]]*("")?[[:space:]]*$'; then
        ERRORS+=("${_CORE_VALUES_LABEL}: nico-api.hostname is empty — set your external nico-api hostname")
    fi
    # Every enabled externalService needs a VIP from the MetalLB pool
    if _strip_comments "${_CORE_VALUES_CFG}" | grep -qE 'loadBalancerIPs:[[:space:]]*("")?[[:space:]]*$'; then
        ERRORS+=("${_CORE_VALUES_LABEL}: one or more loadBalancerIPs are empty — assign each enabled externalService a VIP from your MetalLB pool")
    fi
fi

# MetalLB: a pool declared with no CIDR/range entries
if [[ -f "${_METALLB_CFG}" && ! -d "${_METALLB_CFG}" ]]; then
    if _strip_comments "${_METALLB_CFG}" | grep -qE '^kind:[[:space:]]*IPAddressPool' && \
       ! _strip_comments "${_METALLB_CFG}" | grep -qE '^[[:space:]]*-[[:space:]]*[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+'; then
        ERRORS+=("${_METALLB_CFG_LABEL}: IPAddressPool has no addresses — add your VIP CIDR(s)/range(s)")
    fi
fi

# siteName must be set and not the TMP_SITE default
if [[ -f "${_SITE_VALUES_CFG}" ]]; then
    _sn="$(grep -E '^siteName:' "${_SITE_VALUES_CFG}" | head -1 | sed -E 's/^siteName:[[:space:]]*//; s/["'\'' ]//g')"
    [[ -z "${_sn}" || "${_sn}" == "TMP_SITE" ]] && \
        ERRORS+=("values.yaml: siteName is not set (still '${_sn:-<empty>}')")
fi

# ---------------------------------------------------------------------------
# 3c. IP / subnet validation — every service VIP must be a valid IPv4 that
#     falls inside one of the MetalLB IPAddressPool CIDRs/ranges, and VIPs must
#     not collide. Catches typos and pool/VIP mismatches before MetalLB silently
#     fails to allocate (services stuck <pending>).
# ---------------------------------------------------------------------------
_ip2int() { local a b c d; IFS=. read -r a b c d <<<"$1"; echo $(( (a<<24)+(b<<16)+(c<<8)+d )); }
_is_ipv4() {
    local ip="$1" a b c d
    [[ "$ip" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]] || return 1
    a=${BASH_REMATCH[1]}; b=${BASH_REMATCH[2]}; c=${BASH_REMATCH[3]}; d=${BASH_REMATCH[4]}
    (( a<=255 && b<=255 && c<=255 && d<=255 ))
}
# Does $1 (ip) fall within $2 (CIDR "a.b.c.d/n" or range "a.b.c.d-a.b.c.d")?
_ip_in_block() {
    local ip="$1" block="$2" ipi neti maskbits m start end
    ipi=$(_ip2int "$ip")
    if [[ "$block" == */* ]]; then
        neti=$(_ip2int "${block%/*}"); maskbits="${block#*/}"
        [[ "$maskbits" =~ ^[0-9]+$ ]] || return 2
        m=$(( 0xffffffff ^ ((1 << (32 - maskbits)) - 1) ))
        (( (ipi & m) == (neti & m) ))
    elif [[ "$block" == *-* ]]; then
        start=$(_ip2int "${block%-*}"); end=$(_ip2int "${block#*-}")
        (( ipi >= start && ipi <= end ))
    else
        return 2
    fi
}

if [[ "${SKIP_CORE:-false}" != "true" && -f "${_CORE_VALUES_CFG}" ]]; then
    # Collect the MetalLB pool blocks (CIDRs + ranges) from the rendered config.
    _POOL_BLOCKS=()
    if [[ -n "${_METALLB_RENDERED}" ]]; then
        while IFS= read -r _blk; do
            [[ -n "${_blk}" ]] && _POOL_BLOCKS+=("${_blk}")
        done < <(printf '%s\n' "${_METALLB_RENDERED}" | sed -E 's/[[:space:]]+#.*$//; /^[[:space:]]*#/d' \
                  | grep -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+(/[0-9]+|-[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+)?' \
                  | grep -E '/[0-9]+$|-' )
    fi

    # Collect every configured service VIP (non-empty loadBalancerIPs values).
    _SEEN_VIPS=""
    while IFS= read -r _vip; do
        [[ -z "${_vip}" ]] && continue
        if ! _is_ipv4 "${_vip}"; then
            ERRORS+=("${_CORE_VALUES_LABEL}: loadBalancerIP '${_vip}' is not a valid IPv4 address")
            continue
        fi
        # Duplicate VIP across services
        if [[ " ${_SEEN_VIPS} " == *" ${_vip} "* ]]; then
            WARNINGS+=("${_CORE_VALUES_LABEL}: VIP ${_vip} is assigned to more than one service — each service needs a unique IP")
        fi
        _SEEN_VIPS="${_SEEN_VIPS} ${_vip}"
        # Containment in a MetalLB pool
        if [[ ${#_POOL_BLOCKS[@]} -gt 0 ]]; then
            _in_pool=false
            for _blk in "${_POOL_BLOCKS[@]}"; do
                if _ip_in_block "${_vip}" "${_blk}"; then _in_pool=true; break; fi
            done
            ${_in_pool} || \
                ERRORS+=("${_CORE_VALUES_LABEL}: VIP ${_vip} is not within any MetalLB IPAddressPool (${_METALLB_CFG_LABEL}) — MetalLB cannot allocate it")
        fi
    done < <(sed -E 's/[[:space:]]+#.*$//; /^[[:space:]]*#/d' "${_CORE_VALUES_CFG}" \
              | grep -E 'loadBalancerIPs:' | grep -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' )

    # kea DHCP hook IPs (nameservers / ntpServer / provisioningServer) are handed
    # to DPUs at boot — validate format + pool-containment (no dup check: these
    # intentionally mirror the unbound/ntp/pxe VIPs above).
    while IFS= read -r _kip; do
        [[ -z "${_kip}" ]] && continue
        if ! _is_ipv4 "${_kip}"; then
            ERRORS+=("${_CORE_VALUES_LABEL}: kea hookParameters IP '${_kip}' is not a valid IPv4 address")
        elif [[ ${#_POOL_BLOCKS[@]} -gt 0 ]]; then
            _in_pool=false
            for _blk in "${_POOL_BLOCKS[@]}"; do
                if _ip_in_block "${_kip}" "${_blk}"; then _in_pool=true; break; fi
            done
            ${_in_pool} || \
                WARNINGS+=("${_CORE_VALUES_LABEL}: kea hookParameters IP ${_kip} is not within any MetalLB pool — DPUs will be told to use an unreachable DNS/NTP/PXE endpoint")
        fi
    done < <(sed -E 's/[[:space:]]+#.*$//; /^[[:space:]]*#/d' "${_CORE_VALUES_CFG}" \
              | grep -E 'nameservers:|ntpServer:|provisioningServer:' | grep -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' )

    # Validate MetalLB pool blocks are well-formed CIDR/range.
    for _blk in "${_POOL_BLOCKS[@]}"; do
        _net="${_blk%%[-/]*}"
        if ! _is_ipv4 "${_net}"; then
            ERRORS+=("${_METALLB_CFG_LABEL}: pool entry '${_blk}' is not a valid CIDR/range")
        elif [[ "${_blk}" == */* ]]; then
            _bits="${_blk#*/}"
            { [[ "${_bits}" =~ ^[0-9]+$ ]] && (( _bits >= 0 && _bits <= 32 )); } || \
                ERRORS+=("${_METALLB_CFG_LABEL}: pool entry '${_blk}' has an invalid CIDR prefix length")
        fi
    done
fi

# nico-core: bootArtifactContainers must be populated or DPU/host HTTP boot 404s
if [[ -f "${_CORE_VALUES_CFG}" ]]; then
    if ! sed -E '/^[[:space:]]*#/d' "${_CORE_VALUES_CFG}" | grep -qE 'image:[[:space:]]*\S+.*boot-artifacts|boot-artifacts-'; then
        WARNINGS+=("${_CORE_VALUES_LABEL}: bootArtifactContainers appears empty — DPU/host HTTP boot will 404 (set nico-pxe.bootArtifactContainers)")
    fi
    # If unbound is enabled, it must have localData and an external VIP for DPUs to resolve .forge
    if sed -E '/^[[:space:]]*#/d' "${_CORE_VALUES_CFG}" | grep -qE '^[[:space:]]*enabled:[[:space:]]*true' && \
       sed -E '/^[[:space:]]*#/d' "${_CORE_VALUES_CFG}" | grep -qE '^unbound:'; then
        sed -E '/^[[:space:]]*#/d' "${_CORE_VALUES_CFG}" | grep -qE 'localData:' || \
            WARNINGS+=("${_CORE_VALUES_LABEL}: unbound enabled but no localData (.forge records) set")
    fi
fi

# ---------------------------------------------------------------------------
# 4–7. Cluster checks — all gated on kubectl being available and reachable
# ---------------------------------------------------------------------------
_CLUSTER_REACHABLE=false

if command -v kubectl &>/dev/null; then
    if ! kubectl cluster-info >/dev/null 2>&1; then
        ERRORS+=("Cannot reach the Kubernetes cluster — check KUBECONFIG and cluster connectivity")
    else
        _CLUSTER_REACHABLE=true
    fi
fi

if [[ "${_CLUSTER_REACHABLE}" == "true" ]]; then

    # -----------------------------------------------------------------------
    # 4b. Core image pull secrets — if setup.sh will not create them from
    #     REGISTRY_PULL_SECRET, verify referenced secrets already exist.
    # -----------------------------------------------------------------------
    if [[ "${SKIP_CORE:-false}" != "true" ]]; then
        if [[ -z "${_CORE_IMAGE_PULL_SECRETS}" ]]; then
            if [[ -z "${REGISTRY_PULL_SECRET:-}" ]]; then
                WARNINGS+=("${_CORE_VALUES_LABEL}: no imagePullSecrets found; image pulls must be public or preloaded on every node")
            fi
        else
            _existing_core_pull_secrets=0
            _planned_core_pull_secrets=0
            _missing_core_pull_secrets=""
            for _pull_secret in ${_CORE_IMAGE_PULL_SECRETS}; do
                if kubectl get secret "${_pull_secret}" -n nico-system >/dev/null 2>&1; then
                    _existing_core_pull_secrets=$(( _existing_core_pull_secrets + 1 ))
                elif [[ -n "${REGISTRY_PULL_SECRET:-}" && \
                        ( "${_pull_secret}" == "imagepullsecret" || \
                          "${_pull_secret}" == "nvcr-nico-dev" ) ]]; then
                    _planned_core_pull_secrets=$(( _planned_core_pull_secrets + 1 ))
                else
                    _missing_core_pull_secrets="${_missing_core_pull_secrets}${_missing_core_pull_secrets:+, }${_pull_secret}"
                fi
            done

            if [[ $(( _existing_core_pull_secrets + _planned_core_pull_secrets )) -eq 0 ]]; then
                _core_pull_secret_list="$(printf '%s\n' "${_CORE_IMAGE_PULL_SECRETS}" | tr '\n' ' ' | sed 's/[[:space:]]*$//')"
                if [[ -z "${REGISTRY_PULL_SECRET:-}" ]]; then
                    ERRORS+=("REGISTRY_PULL_SECRET is not set and ${_CORE_VALUES_LABEL} references imagePullSecrets (${_core_pull_secret_list}), but none exist in nico-system — set REGISTRY_PULL_SECRET, pre-create the pull secret(s), or remove imagePullSecrets for an unauthenticated registry")
                else
                    ERRORS+=("${_CORE_VALUES_LABEL} references imagePullSecrets (${_core_pull_secret_list}), but none exist in nico-system and setup.sh will not create those names")
                fi
            elif [[ -n "${_missing_core_pull_secrets}" ]]; then
                WARNINGS+=("${_CORE_VALUES_LABEL}: imagePullSecrets not found in nico-system: ${_missing_core_pull_secrets}")
            fi
        fi
    fi

    # -----------------------------------------------------------------------
    # 5. Node resources — at least 3 schedulable nodes required
    # -----------------------------------------------------------------------
    _schedulable=$(kubectl get nodes -o json 2>/dev/null | jq -r '
        .items[] |
        select(
            (.status.conditions[] | select(.type == "Ready") | .status) == "True" and
            ((.spec.taints // []) |
             map(select(.effect == "NoSchedule" or .effect == "NoExecute")) |
             length) == 0
        ) | .metadata.name' | wc -l | tr -d '[:space:]')

    _total=$(kubectl get nodes --no-headers 2>/dev/null | wc -l | tr -d '[:space:]')

    if [[ "${_schedulable}" -lt 3 ]]; then
        ERRORS+=("Only ${_schedulable}/${_total} nodes are schedulable (Ready + untainted) — at least 3 required for HA Vault and Postgres")
    fi

    # -----------------------------------------------------------------------
    # 6. MetalLB BGPPeer node hostnames — verify they exist in this cluster
    #
    # Extracts node names listed under kubernetes.io/hostname in BGPPeer
    # nodeSelectors and checks each one against the actual cluster node list.
    # -----------------------------------------------------------------------
    if [[ -n "${_METALLB_RENDERED}" ]]; then
        _cluster_nodes=$(kubectl get nodes \
            -o jsonpath='{.items[*].metadata.name}' 2>/dev/null)
        # Extract nodeSelector hostnames from BGPPeer resources. Supports both:
        #   matchLabels: kubernetes.io/hostname: node-a
        #   matchExpressions: key: kubernetes.io/hostname, values: [node-a]
        _peer_nodes=$(printf '%s\n' "${_METALLB_RENDERED}" | awk '
            /^---[[:space:]]*$/ {
                in_bgp=0; saw_hostname_key=0; collect_values=0; next
            }
            /^[[:space:]]*kind:[[:space:]]*BGPPeer[[:space:]]*$/ {
                in_bgp=1; next
            }
            !in_bgp { next }
            /^[[:space:]]*kubernetes\.io\/hostname:[[:space:]]*/ {
                val=$0
                sub(/^[[:space:]]*kubernetes\.io\/hostname:[[:space:]]*/, "", val)
                gsub(/#.*$/, "", val)
                gsub(/"/, "", val)
                gsub(/\047/, "", val)
                gsub(/[[:space:]]/, "", val)
                if (length > 0) print val
                next
            }
            /^[[:space:]]*-[[:space:]]*key:[[:space:]]*kubernetes\.io\/hostname[[:space:]]*$/ {
                saw_hostname_key=1; collect_values=0; next
            }
            saw_hostname_key && /^[[:space:]]*values:[[:space:]]*$/ {
                collect_values=1; next
            }
            collect_values && /^[[:space:]]*-[[:space:]]+[^-]/ {
                val=$0
                sub(/^[[:space:]]*-[[:space:]]+/, "", val)
                gsub(/#.*$/, "", val)
                gsub(/"/, "", val)
                gsub(/\047/, "", val)
                gsub(/[[:space:]]/, "", val)
                if (length > 0) print val
                next
            }
            collect_values && $0 !~ /^[[:space:]]*($|#|-)/ {
                saw_hostname_key=0; collect_values=0
            }
        ')

        for _peer_node in ${_peer_nodes}; do
            if ! echo " ${_cluster_nodes} " | grep -qF " ${_peer_node} "; then
                WARNINGS+=("${_METALLB_CFG_LABEL}: BGPPeer references node '${_peer_node}' which was not found in the cluster — run: kubectl get nodes")
            fi
        done
    fi

    # -----------------------------------------------------------------------
    # 7. Per-node checks — kernel parameters + DNS
    #
    # One pod per node using:
    #   hostPID: true      — lets nsenter reach host PID 1's namespaces
    #   privileged: true   — required for nsenter -n (network namespace entry)
    #
    # nsenter -t 1 -n reads sysctl values from the host's network namespace,
    # not the container's (which always has ip_forward=0 by default).
    # The DNS lookup runs in the container's own network namespace so it
    # uses cluster DNS (CoreDNS), not the host's /etc/resolv.conf.
    #
    # Pods are deleted after logs are collected.
    # Override the check image for air-gapped clusters:
    #   export PREFLIGHT_CHECK_IMAGE=my-registry.example.com/busybox:1.36
    # -----------------------------------------------------------------------
    _CHECK_IMAGE="${PREFLIGHT_CHECK_IMAGE:-busybox:1.36}"
    _TS="$(date +%s)"
    _node_names=$(kubectl get nodes \
        -o jsonpath='{.items[*].metadata.name}' 2>/dev/null)

    for _node in ${_node_names}; do
        # Lowercase via tr for portability (bash 3.2 on macOS lacks ${var,,}).
        _safe="$(printf '%s' "${_node}" | tr '[:upper:]' '[:lower:]')"
        _safe="${_safe//[^a-z0-9-]/-}"
        _safe="${_safe:0:40}"
        _pod="nico-pf-${_TS}-${_safe}"
        _PREFLIGHT_PODS+=("${_pod}")

        kubectl apply -f - >/dev/null 2>&1 <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: ${_pod}
  namespace: ${_PREFLIGHT_NS}
  labels:
    nico-preflight: "true"
spec:
  nodeName: ${_node}
  hostPID: true
  restartPolicy: Never
  tolerations:
  - operator: Exists
  containers:
  - name: check
    image: ${_CHECK_IMAGE}
    securityContext:
      privileged: true
    command:
    - sh
    - -c
    - |
      printf "NODE=${_node}\n"
      printf "bridge_nf=%s\n" "\$(nsenter -t 1 -n -- sysctl -n net.bridge.bridge-nf-call-iptables 2>/dev/null || echo MISSING)"
      printf "ip_forward=%s\n" "\$(nsenter -t 1 -n -- sysctl -n net.ipv4.ip_forward 2>/dev/null || echo MISSING)"
      nslookup kubernetes.default.svc.cluster.local >/dev/null 2>&1 \
        && printf "dns=ok\n" || printf "dns=FAIL\n"
    resources:
      requests:
        cpu: 10m
        memory: 16Mi
EOF
    done

    echo "Running per-node checks (sysctl, DNS) across ${#_PREFLIGHT_PODS[@]} node(s)..."

    # Wait up to 120s for all pods to reach Succeeded or Failed
    _deadline=$(( $(date +%s) + 120 ))
    while [[ $(date +%s) -lt "${_deadline}" ]]; do
        _pending=0
        for _pod in "${_PREFLIGHT_PODS[@]}"; do
            _phase=$(kubectl get pod "${_pod}" -n "${_PREFLIGHT_NS}" \
                -o jsonpath='{.status.phase}' 2>/dev/null || echo "Unknown")
            [[ "${_phase}" != "Succeeded" && "${_phase}" != "Failed" ]] && \
                (( _pending++ )) || true
        done
        [[ "${_pending}" -eq 0 ]] && break
        sleep 5
    done

    # Parse and report results
    for _pod in "${_PREFLIGHT_PODS[@]}"; do
        _logs=$(kubectl logs "${_pod}" -n "${_PREFLIGHT_NS}" 2>/dev/null || true)
        _node_label=$(echo "${_logs}" | grep '^NODE='       | cut -d= -f2-)
        _bridge_nf=$( echo "${_logs}" | grep '^bridge_nf='  | cut -d= -f2-)
        _ip_fwd=$(    echo "${_logs}" | grep '^ip_forward='  | cut -d= -f2-)
        _dns=$(       echo "${_logs}" | grep '^dns='         | cut -d= -f2-)
        _label="${_node_label:-${_pod}}"

        if [[ -z "${_logs}" ]]; then
            WARNINGS+=("Node ${_label}: per-node check produced no output — possible image pull timeout; set PREFLIGHT_CHECK_IMAGE to a pre-pulled local image")
            continue
        fi

        [[ "${_bridge_nf}" != "1" ]] && \
            ERRORS+=("Node ${_label}: net.bridge.bridge-nf-call-iptables=${_bridge_nf:-MISSING}  (fix: sysctl -w net.bridge.bridge-nf-call-iptables=1)")
        [[ "${_ip_fwd}" != "1" ]] && \
            ERRORS+=("Node ${_label}: net.ipv4.ip_forward=${_ip_fwd:-MISSING}  (fix: sysctl -w net.ipv4.ip_forward=1)")
        [[ "${_dns}" != "ok" ]] && \
            WARNINGS+=("Node ${_label}: DNS resolution failed for kubernetes.default.svc.cluster.local — check CoreDNS: kubectl get pods -n kube-system -l k8s-app=kube-dns")
    done

    _cleanup_preflight_pods

fi  # _CLUSTER_REACHABLE

# ---------------------------------------------------------------------------
# 8. Registry connectivity — treat any HTTP response as reachable;
#    only warn on connection failure (HTTP 000 = could not connect at all)
# ---------------------------------------------------------------------------
if [[ -n "${NICO_IMAGE_REGISTRY:-}" ]] && command -v curl &>/dev/null; then
    _reg_host="${NICO_IMAGE_REGISTRY%%/*}"
    _http_code=$(curl --connect-timeout 5 --max-time 10 \
        -o /dev/null -w "%{http_code}" \
        "https://${_reg_host}/v2/" 2>/dev/null || echo "000")
    if [[ "${_http_code}" == "000" ]]; then
        WARNINGS+=("Registry '${_reg_host}' is not reachable (connection failed) — check network access; image pulls will fail")
    fi
fi

# ---------------------------------------------------------------------------
# 9. NICo REST source tree and Helm charts (in-tree)
#
# The REST stack lives in this repo under rest-api/. No separate clone is
# supported any more; the legacy NICO_REST_REPO / NICO_REPO env vars and the
# sibling-directory fallbacks were removed once rest-api/ became part of
# core. The REST Helm charts live under helm/rest/. If either path is missing,
# the checkout is broken — error out so the user fixes it rather than
# installing a half-stack.
# ---------------------------------------------------------------------------
NICO_REST_DIR=""
NICO_REST_HELM_DIR=""
_NICO_REST_ENABLED=true
[[ "${SKIP_REST:-false}" == "true" ]] && _NICO_REST_ENABLED=false

if ${_NICO_REST_ENABLED}; then
    _NICO_REST_CANDIDATE="${SCRIPT_DIR}/../rest-api"
    _NICO_REST_HELM_CANDIDATE="${SCRIPT_DIR}/../helm/rest"
    if [[ -d "${_NICO_REST_CANDIDATE}" ]]; then
        NICO_REST_DIR="$(cd "${_NICO_REST_CANDIDATE}" && pwd)"
    else
        ERRORS+=("rest-api/ not found at ${_NICO_REST_CANDIDATE} — check out the full core repo, or pass --skip-rest if you only need the infra prereqs.")
    fi
    if [[ -d "${_NICO_REST_HELM_CANDIDATE}/nico-rest" && -d "${_NICO_REST_HELM_CANDIDATE}/nico-rest-site-agent" ]]; then
        NICO_REST_HELM_DIR="$(cd "${_NICO_REST_HELM_CANDIDATE}" && pwd)"
    else
        ERRORS+=("REST Helm charts not found under ${_NICO_REST_HELM_CANDIDATE} — expected nico-rest and nico-rest-site-agent charts.")
    fi
fi

# ---------------------------------------------------------------------------
# Output and prompts
# ---------------------------------------------------------------------------
_print_separator() { echo "---------------------------------------------------------------------"; }

if [[ ${#ERRORS[@]} -eq 0 && ${#WARNINGS[@]} -eq 0 ]]; then
    if ${_NICO_REST_ENABLED}; then
        echo "Pre-flight OK  (NICo REST source: ${NICO_REST_DIR}, charts: ${NICO_REST_HELM_DIR})"
    else
        echo "Pre-flight OK  (NICo REST skipped)"
    fi
    if ${_SOURCED}; then return 0; else exit 0; fi
fi

echo ""
_print_separator
echo "  PRE-FLIGHT CHECK RESULTS"
_print_separator

if [[ ${#ERRORS[@]} -gt 0 ]]; then
    echo ""
    echo "  ERRORS (setup will fail without these):"
    for _e in "${ERRORS[@]}"; do
        echo "    ✗  ${_e}"
    done
fi

if [[ ${#WARNINGS[@]} -gt 0 ]]; then
    echo ""
    echo "  WARNINGS (setup may fail or be incomplete):"
    for _w in "${WARNINGS[@]}"; do
        echo "    ⚠  ${_w}"
    done
fi

echo ""
_print_separator

# Warnings only — default continue
if [[ ${#ERRORS[@]} -eq 0 ]]; then
    if [[ "${AUTO_YES:-false}" == "true" ]]; then
        echo "  Warnings noted — continuing."
    else
        echo ""
        read -r -p "  ➤  Warnings above noted. Continue anyway? [Y/n]: " _reply
        echo ""
        if [[ ! "${_reply:-Y}" =~ ^[Yy]$ ]]; then
            echo "  Aborted."
            if ${_SOURCED}; then return 1; else exit 1; fi
        fi
    fi
    if ${_SOURCED}; then return 0; else exit 0; fi
fi

# Hard errors — default abort
if [[ "${AUTO_YES:-false}" == "true" ]]; then
    echo "  Errors above noted — continuing (-y flag set). Things may fail."
    if ${_SOURCED}; then return 0; else exit 0; fi
fi

echo ""
echo "  The issues above will likely cause setup to fail."
echo ""
read -r -p "  ➤  Continue anyway at your own risk? [y/N]: " _reply
echo ""
if [[ "${_reply:-N}" =~ ^[Yy]$ ]]; then
    echo "  Continuing — good luck."
    if ${_SOURCED}; then return 0; else exit 0; fi
fi

echo "  Fix the issues above and re-run setup.sh."
if ${_SOURCED}; then return 1; else exit 1; fi
