#!/bin/bash
# Tests for fnn.routeTargetsToImport (#6411): the build script validates the key
# under `fnn`, and startupSMN.template must read it from the same place. Before
# the fix the template read it from the top level, so the placement the script
# accepted was silently dropped from the rendered startup.yaml and the placement
# the template read was rejected by the script.
#
# Renders the real template with gomplate the way build-dpu-install-iso.sh does
# (context = vars file, datasource "site" = the site yaml) and sources the
# script's own _check_unknown_keys block for the validation half.

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"

for tool in gomplate yq; do
    if ! command -v "$tool" &>/dev/null; then
        echo "SKIP: $tool not installed"
        exit 0
    fi
done

SCRIPT="$UNIT_TEST_DIR/../build-dpu-install-iso.sh"
TEMPLATE="$UNIT_TEST_DIR/../on-server/templates/startupSMN.template"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

DC_ASN=4266030000

# ── vars file: every {{.Var}} the template uses gets a value ────────────────
VARS="$TMP/vars.yaml"
grep -o '{{[^}]*}}' "$TEMPLATE" | grep -o '\.[A-Z][A-Za-z0-9]*' | sort -u | sed 's/^\.//' \
    | while read -r v; do printf '%s: "x"\n' "$v"; done > "$VARS"
yq -i ".DatacenterASN = \"$DC_ASN\"
     | .FnnCommonManagedNodeBmcRouteTarget = \"900\"
     | .FnnCommonAdminNetworkTarget = \"50400\"
     | .FnnCommonSiteControllerRouteTarget = \"50100\"" "$VARS"

render() {   # render <site-yaml> → rendered startup.yaml on stdout
    gomplate --file "$TEMPLATE" --context ".=${VARS}?type=application/yaml" --datasource "site=$1"
}

# Keys of the control-plane VRF's from-evpn route-target map, in rendered order.
imported_targets() {   # imported_targets <rendered-file>
    awk '/from-evpn:/{f=1; next} f && /route-target:/{r=1; next} r && /auto: \{\}/{exit} r && /^ *[^ #].*: *\{\}/{sub(/^ */,""); sub(/:? *\{\}.*$/,""); print}' "$1"
}

# ── site files ───────────────────────────────────────────────────────────────
base_site() {   # base_site <file> ; a minimal FNN site file (only what the check and the template read)
    cat > "$1" <<EOF
datacenterAsn: $DC_ASN
siteControllerRoutesAsn: $DC_ASN
bgpAsnStart: 4200100000
siteControllerMtuSize: 9000
forgeDpuLoopbackPrefix: 10.10.0.0/28
forgeServiceVipPrefix: 10.10.2.0/27
forgeControlPlanePrefix: 10.10.1.0/29
nameServer: 10.0.0.53
ubuntuPasswordHash: x
siteControllerNodes:
  - hostName: sc1
    mac: aa:aa:aa:aa:aa:aa
fnn:
  controlPlaneVni: 60000
  commonManagedNodeBmcRouteTarget: 900
  commonSiteControllerRouteTarget: 50100
  commonAdminNetworkTarget: 50400
EOF
}

SITE_NONE="$TMP/site-none.yaml";   base_site "$SITE_NONE"
SITE_NESTED="$TMP/site-nested.yaml"; base_site "$SITE_NESTED"
cat >> "$SITE_NESTED" <<EOF
  routeTargetsToImport:
    $DC_ASN:101: {}
    $DC_ASN:1003: {}
EOF
SITE_TOP="$TMP/site-top.yaml"; base_site "$SITE_TOP"
cat >> "$SITE_TOP" <<EOF
routeTargetsToImport:
  $DC_ASN:101: {}
EOF

# ── template: renders the nested key, ignores the top-level one ─────────────
echo "=== startupSMN.template: fnn.routeTargetsToImport is rendered ==="

render "$SITE_NESTED" > "$TMP/nested.yaml"
got=(); while IFS= read -r l; do got+=("$l"); done < <(imported_targets "$TMP/nested.yaml")
assert_eq "five imports: three fixed + two additional" 5 "${#got[@]}"
assert_eq "1st import is the managed-node BMC tag"     "$DC_ASN:900"   "${got[0]:-}"
assert_eq "2nd import is the admin network tag"        "$DC_ASN:50400" "${got[1]:-}"
assert_eq "3rd import is the site controller tag"      "$DC_ASN:50100" "${got[2]:-}"
extras="$(printf '%s\n' "${got[@]:3}" | sort | tr '\n' ' ')"
assert_eq "additional targets follow the fixed three, before auto" "$DC_ASN:1003 $DC_ASN:101 " "$extras"
assert_true "auto follows the additional targets" \
    "grep -A1 -F '$DC_ASN:101: {}' '$TMP/nested.yaml' | grep -q 'auto: {}' || grep -A1 -F '$DC_ASN:1003: {}' '$TMP/nested.yaml' | grep -q 'auto: {}'"
assert_true "rendered file is valid YAML" "yq -e . '$TMP/nested.yaml' >/dev/null"

echo ""
echo "=== startupSMN.template: without the key, exactly the three fixed imports ==="
render "$SITE_NONE" > "$TMP/none.yaml"
got=(); while IFS= read -r l; do got+=("$l"); done < <(imported_targets "$TMP/none.yaml")
assert_eq "three imports" 3 "${#got[@]}"
assert_eq "no additional target rendered" "0" "$(printf '%s\n' "${got[@]}" | grep -c ':101$' || true)"

echo ""
echo "=== startupSMN.template: a top-level key is not read (the script rejects it anyway) ==="
render "$SITE_TOP" > "$TMP/top.yaml"
got=(); while IFS= read -r l; do got+=("$l"); done < <(imported_targets "$TMP/top.yaml")
assert_eq "three imports" 3 "${#got[@]}"
assert_false "top-level target absent from the render" "grep -q -F '$DC_ASN:101' '$TMP/top.yaml'"

# ── build script validation: nested accepted, top-level rejected ─────────────
# Source the script's own _check_unknown_keys function and the two calls that
# use it (the "Validating site config fields" step), so the test cannot drift
# from the lists the script really enforces.
echo ""
echo "=== build-dpu-install-iso.sh: key validation ==="
CHECK="$TMP/check.sh"
{
    echo 'die()  { echo "ERROR: $*" >&2; exit 1; }'
    echo 'step() { :; }'
    awk '/^_check_unknown_keys\(\) \{/{f=1} f{print} f && /^\}/{exit}' "$SCRIPT"
    awk '/^step "Validating site config fields"/{f=1; next} f{print} f && /^fi$/{exit}' "$SCRIPT"
} > "$CHECK"
assert_true "extracted the check function from the script"  "grep -q '^_check_unknown_keys() {' '$CHECK'"
assert_true "extracted the fnn key list from the script"    "grep -q '_check_unknown_keys \"fnn\"' '$CHECK'"

validate() { CONTROL_PLANE_CONFIG="$1" bash "$CHECK" 2>"$TMP/err"; }

assert_true  "fnn.routeTargetsToImport passes validation"      "validate '$SITE_NESTED'"
assert_true  "no routeTargetsToImport passes validation"       "validate '$SITE_NONE'"
assert_false "top-level routeTargetsToImport fails validation" "validate '$SITE_TOP'"
assert_true  "the rejection names the key" "grep -q \"Unsupported field in site config (top level): 'routeTargetsToImport'\" '$TMP/err'"

# ── the shipped sample, with its commented block enabled ─────────────────────
echo ""
echo "=== site-sample.yaml: enabling the commented block yields a working site file ==="
SAMPLE="$UNIT_TEST_DIR/../site-sample.yaml"
SITE_SAMPLE="$TMP/site-sample.yaml"
sed -E 's/^  #(routeTargetsToImport:)/  \1/; s/^  #(  [0-9]+:[0-9]+: \{\})/  \1/' "$SAMPLE" > "$SITE_SAMPLE"
assert_true "sample has the block under fnn"      "yq -e '.fnn.routeTargetsToImport | length > 0' '$SITE_SAMPLE' >/dev/null"
assert_eq   "sample keys are all numeric <asn>:<n>" 0 "$(yq -r '.fnn.routeTargetsToImport | keys | .[]' "$SITE_SAMPLE" | grep -cvE '^[0-9]+:[0-9]+$' || true)"
assert_true "sample passes validation"            "validate '$SITE_SAMPLE'"
render "$SITE_SAMPLE" > "$TMP/sample.yaml"
sample_asn="$(yq -r '.datacenterAsn' "$SAMPLE")"
assert_true "sample's targets are rendered"       "grep -q -F '$sample_asn:101: {}' '$TMP/sample.yaml'"

summary
