#!/bin/bash
# Tests for the BGP leaf-session password rendered at build time
# (build-dpu-install-iso.sh → startupSMN.template / startup.template, installWithLeafPassword):
#   - password given: `password:` under both ToR neighbours (FNN) / the HBNUNDERLAY peer group (non-FNN)
#   - password empty or variable absent: no `password:` key, output byte-identical to the
#     templates before this feature (git upstream/main when available)
#   - passwords containing " and \ round-trip through the vars file and the template
#   - rendered output is valid YAML

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"

TEMPLATES_DIR="$UNIT_TEST_DIR/../on-server/templates"

if ! command -v gomplate >/dev/null 2>&1 || ! command -v yq >/dev/null 2>&1; then
    echo "  SKIP: gomplate and yq are required for render tests"
    exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
SITE="$WORK/site.yaml"
printf 'siteControllerNodes: []\n' > "$SITE"

# Mirrors render_template() in build-dpu-install-iso.sh, including its escaping of
# backslash and double quote for the YAML vars file.
render() {
    local tmpl="$1"; shift
    local vars="$WORK/vars.yaml"
    : > "$vars"
    while [[ $# -gt 0 ]]; do
        local val="${1#*=}"
        val="${val//\\/\\\\}"
        val="${val//\"/\\\"}"
        printf '%s: "%s"\n' "${1%%=*}" "$val" >> "$vars"
        shift
    done
    gomplate --file "$tmpl" --context ".=${vars}?type=application/yaml" --datasource "site=$SITE"
}

COMMON=(
    "ASN=4200100001" "LoopbackIP=10.10.0.1" "Loopback=10.10.0.1/32" "Hostnet=10.10.1.0/31"
    "RemoteAS=4200100000" "BGPNeighbor=10.10.1.1" "Rule30=10.10.2.0/28" "Rule40=10.10.2.16/28"
)
FNN=(
    "ControlPlaneVNI=60000" "SiteControllerRoutesASN=4200000100" "DatacenterASN=4200000100"
    "VpcVrfLoopback=" "FnnCommonManagedNodeBmcRouteTarget=900"
    "FnnCommonSiteControllerRouteTarget=50100" "FnnCommonAdminNetworkTarget=50400"
)
# p0_if/p1_if are also interface names; pick the map that carries a password.
pw_of() { yq ".. | select(has(\"$2\")) | .$2.password | select(. != null)" "$1"; }

echo "=== FNN template, password given ==="
out="$WORK/fnn.yaml"
render "$TEMPLATES_DIR/startupSMN.template" "${COMMON[@]}" "${FNN[@]}" "BgpLeafSessionPassword=s3cret" > "$out"
assert_true "renders valid YAML" "yq '.' '$out' >/dev/null"
assert_eq   "p0_if has the password" "s3cret" "$(pw_of "$out" p0_if)"
assert_eq   "p1_if has the password" "s3cret" "$(pw_of "$out" p1_if)"
assert_eq   "exactly two password keys" "2" "$(grep -c '^ *password:' "$out")"

echo ""
echo "=== non-FNN template, password given ==="
out="$WORK/plain.yaml"
render "$TEMPLATES_DIR/startup.template" "${COMMON[@]}" "BgpLeafSessionPassword=s3cret" > "$out"
assert_true "renders valid YAML" "yq '.' '$out' >/dev/null"
assert_eq   "HBNUNDERLAY peer-group has the password" "s3cret" "$(pw_of "$out" HBNUNDERLAY)"
assert_eq   "exactly one password key" "1" "$(grep -c '^ *password:' "$out")"

echo ""
echo "=== no password: no key, and identical to the pre-feature render ==="
for tmpl in startupSMN.template startup.template; do
    args=("${COMMON[@]}")
    [[ "$tmpl" == startupSMN.template ]] && args+=("${FNN[@]}")
    empty="$WORK/$tmpl.empty.yaml"; absent="$WORK/$tmpl.absent.yaml"
    render "$TEMPLATES_DIR/$tmpl" "${args[@]}" "BgpLeafSessionPassword=" > "$empty"
    render "$TEMPLATES_DIR/$tmpl" "${args[@]}" > "$absent"
    assert_true  "$tmpl: empty password renders valid YAML" "yq '.' '$empty' >/dev/null"
    assert_false "$tmpl: empty password → no password key" "grep -q '^ *password:' '$empty'"
    assert_true  "$tmpl: variable absent renders (no missing-key error)" "[ -s '$absent' ]"
    assert_true  "$tmpl: empty and absent renders identical" "cmp -s '$empty' '$absent'"
    # Against the template as it was before this feature, when git history is available.
    if git -C "$UNIT_TEST_DIR" show "upstream/main:scripts/control-plane-dpu/on-server/templates/$tmpl" > "$WORK/$tmpl.old" 2>/dev/null; then
        render "$WORK/$tmpl.old" "${args[@]}" > "$WORK/$tmpl.old.yaml"
        assert_true "$tmpl: byte-identical to upstream/main render" "cmp -s '$WORK/$tmpl.old.yaml' '$empty'"
    else
        echo "  (skipped upstream/main comparison — ref not available)"
    fi
done

echo ""
echo "=== awkward characters round-trip ==="
pw='a"b\c'"'"'d e'
out="$WORK/quoted.yaml"
render "$TEMPLATES_DIR/startup.template" "${COMMON[@]}" "BgpLeafSessionPassword=$pw" > "$out"
assert_true "renders valid YAML" "yq '.' '$out' >/dev/null"
assert_eq   "password round-trips through vars file and template" "$pw" "$(pw_of "$out" HBNUNDERLAY)"
long=$(printf 'x%.0s' $(seq 1 80))
render "$TEMPLATES_DIR/startup.template" "${COMMON[@]}" "BgpLeafSessionPassword=$long" > "$out"
assert_eq   "80-character password intact" "$long" "$(pw_of "$out" HBNUNDERLAY)"

summary
