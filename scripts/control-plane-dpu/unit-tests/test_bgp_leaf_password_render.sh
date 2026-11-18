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
BUILD_SH="$UNIT_TEST_DIR/../build-dpu-install-iso.sh"

if ! command -v gomplate >/dev/null 2>&1 || ! command -v yq >/dev/null 2>&1; then
    echo "  SKIP: gomplate and yq are required for render tests"
    exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
SITE="$WORK/site.yaml"
printf 'siteControllerNodes: []\n' > "$SITE"

# The shipped renderer, sourced verbatim from build-dpu-install-iso.sh so that its
# escaping of backslash and double quote for the YAML vars file is what gets tested.
# It writes the vars file under OUTPUT_DIR and reads the site file from CONTROL_PLANE_CONFIG.
die() { echo "ERROR: $*" >&2; exit 1; }   # the renderer's refusal path; run it in a subshell
eval "$(sed -n '/^# >>> template-renderer/,/^# <<< template-renderer/p' "$BUILD_SH")"
declare -F render_template >/dev/null || { echo "  FAIL: render_template not found in $BUILD_SH"; exit 1; }
OUTPUT_DIR="$WORK"
CONTROL_PLANE_CONFIG="$SITE"

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
render_template "$TEMPLATES_DIR/startupSMN.template" "${COMMON[@]}" "${FNN[@]}" "BgpLeafSessionPassword=s3cret" > "$out"
assert_true "renders valid YAML" "yq '.' '$out' >/dev/null"
assert_eq   "p0_if has the password" "s3cret" "$(pw_of "$out" p0_if)"
assert_eq   "p1_if has the password" "s3cret" "$(pw_of "$out" p1_if)"
assert_eq   "exactly two password keys" "2" "$(grep -c '^ *password:' "$out")"

echo ""
echo "=== non-FNN template, password given ==="
out="$WORK/plain.yaml"
render_template "$TEMPLATES_DIR/startup.template" "${COMMON[@]}" "BgpLeafSessionPassword=s3cret" > "$out"
assert_true "renders valid YAML" "yq '.' '$out' >/dev/null"
assert_eq   "HBNUNDERLAY peer-group has the password" "s3cret" "$(pw_of "$out" HBNUNDERLAY)"
assert_eq   "exactly one password key" "1" "$(grep -c '^ *password:' "$out")"

echo ""
echo "=== no password: no key, and identical to the pre-feature render ==="
for tmpl in startupSMN.template startup.template; do
    args=("${COMMON[@]}")
    [[ "$tmpl" == startupSMN.template ]] && args+=("${FNN[@]}")
    empty="$WORK/$tmpl.empty.yaml"; absent="$WORK/$tmpl.absent.yaml"
    render_template "$TEMPLATES_DIR/$tmpl" "${args[@]}" "BgpLeafSessionPassword=" > "$empty"
    render_template "$TEMPLATES_DIR/$tmpl" "${args[@]}" > "$absent"
    assert_true  "$tmpl: empty password renders valid YAML" "yq '.' '$empty' >/dev/null"
    assert_false "$tmpl: empty password → no password key" "grep -q '^ *password:' '$empty'"
    assert_true  "$tmpl: variable absent renders (no missing-key error)" "[ -s '$absent' ]"
    assert_true  "$tmpl: empty and absent renders identical" "cmp -s '$empty' '$absent'"
    # Against the template as it was before this feature, when git history is available.
    if git -C "$UNIT_TEST_DIR" show "upstream/main:scripts/control-plane-dpu/on-server/templates/$tmpl" > "$WORK/$tmpl.old" 2>/dev/null; then
        render_template "$WORK/$tmpl.old" "${args[@]}" > "$WORK/$tmpl.old.yaml"
        assert_true "$tmpl: byte-identical to upstream/main render" "cmp -s '$WORK/$tmpl.old.yaml' '$empty'"
    else
        echo "  (skipped upstream/main comparison — ref not available)"
    fi
done

echo ""
echo "=== awkward characters round-trip ==="
pw='a"b\c'"'"'d e'
out="$WORK/quoted.yaml"
render_template "$TEMPLATES_DIR/startup.template" "${COMMON[@]}" "BgpLeafSessionPassword=$pw" > "$out"
assert_true "renders valid YAML" "yq '.' '$out' >/dev/null"
assert_eq   "password round-trips through vars file and template" "$pw" "$(pw_of "$out" HBNUNDERLAY)"
long=$(printf 'x%.0s' $(seq 1 80))
render_template "$TEMPLATES_DIR/startup.template" "${COMMON[@]}" "BgpLeafSessionPassword=$long" > "$out"
assert_eq   "80-character password intact" "$long" "$(pw_of "$out" HBNUNDERLAY)"

echo ""
echo "=== control characters: refused by the renderer and by the validation ==="
# A raw carriage return inside a double-quoted YAML scalar is a line break that
# YAML folds into a space; the renderer refuses any control character rather
# than hand YAML something it would alter.
for probe in 'carriage return:abc\rdef' 'newline:abc\ndef' 'tab:abc\tdef' 'Ctrl-A:abc\x01def' \
             'U+0085 NEL:abc\xc2\x85def' 'U+2028 LS:abc\xe2\x80\xa8def' 'U+2029 PS:abc\xe2\x80\xa9def'; do
    label="${probe%%:*}"; val="$(printf '%b' "${probe#*:}")"
    assert_false "a value with a $label is refused by the renderer" \
        "( render_template '$TEMPLATES_DIR/startup.template' \"\${COMMON[@]}\" \"BgpLeafSessionPassword=\$val\" >/dev/null 2>'$WORK/err' )"
done
assert_true  "the refusal names the variable" "grep -q \"render_template: variable 'BgpLeafSessionPassword' contains a control or line-separator character\" '$WORK/err'"
assert_eq    "a refused render leaves no vars file behind" 0 "$(ls "$WORK"/gomplate_vars_* 2>/dev/null | wc -l | tr -d ' ')"

# The build's own validation block, sourced verbatim, with its inputs stubbed.
VALIDATE="$WORK/validate.sh"
{
    echo 'die() { echo "ERROR: $*" >&2; exit 1; }'
    echo 'log() { :; }'
    echo 'have_tty() { return 1; }'
    echo 'INSTALL_WITH_LEAF_PASSWORD=true'
    sed -n '/^# >>> leaf-password-validation/,/^# <<< leaf-password-validation/p' "$BUILD_SH"
} > "$VALIDATE"
# Guard the extraction itself (a previous test here passed vacuously when its
# extraction stopped early): the block must be complete, parse, and contain
# the check this test is about.
assert_true "extracted the validation block from the build script" "grep -q 'BGP leaf session password must be' '$VALIDATE'"
assert_true "the extracted block runs to its closing marker"       "grep -q '^# <<< leaf-password-validation' '$VALIDATE'"
assert_true "the extracted block contains the printable-ASCII check" "grep -qF '[![:print:]]' '$VALIDATE'"
assert_true "the extracted block parses as bash"                    "bash -n '$VALIDATE'"
validate_pw() { BGP_LEAF_SESSION_PASSWORD="$1" bash "$VALIDATE" 2>"$WORK/verr"; }
# A control character must reach the block intact through the environment, or
# a rejection would prove nothing; the byte-count case below checks that.
assert_true  "a printable password passes validation"             "validate_pw 's3cret'"
# Spaces are allowed anywhere, including at the ends (README: "space included"); nothing trims them.
assert_true  "surrounding spaces are accepted"                     "validate_pw '  s3cret  '"
assert_eq    "and kept as given"                                   "  s3cret  " "$(BGP_LEAF_SESSION_PASSWORD='  s3cret  ' bash -c "source '$VALIDATE'; printf '%s' \"\$BGP_LEAF_SESSION_PASSWORD\"" 2>/dev/null)"
assert_false "a password with a carriage return fails validation" "validate_pw \$'abc\\rdef'"
assert_true  "the rejection says printable ASCII single line"     "grep -q 'must be a single line of printable ASCII characters' '$WORK/verr'"
assert_eq    "the carriage return reached the block (7 bytes seen, not 6)" 7 "$(BGP_LEAF_SESSION_PASSWORD=$'abc\rdef' bash -c 'LC_ALL=C printf "%s" "$BGP_LEAF_SESSION_PASSWORD" | wc -c | tr -d " "')"
assert_false "a password with a newline still fails validation"   "validate_pw \$'abc\\ndef'"
assert_false "a password with a tab fails validation"             "validate_pw \$'abc\\tdef'"
assert_false "a password with Ctrl-B fails validation"            "validate_pw \$'abc\\x02def'"
assert_false "a password with DEL (0x7f) fails validation"        "validate_pw \$'abc\\x7fdef'"
assert_false "a password with non-ASCII text fails validation"    "validate_pw \$'caf\\xc3\\xa9'"
assert_false "a password with an invalid byte fails validation"   "validate_pw \$'abc\\xffdef'"
assert_true  "a password with a space passes validation"          "validate_pw 'pass word'"
all_print="$(LC_ALL=C awk 'BEGIN{for(i=32;i<=111;i++) printf "%c", i}')"   # 80 printable ASCII bytes, space to o
assert_eq   "the printable sample is 80 bytes"                   80 "$(LC_ALL=C printf '%s' "$all_print" | wc -c | tr -d ' ')"
assert_true "a password of printable ASCII passes validation"   "validate_pw \"\$all_print\""
assert_false "81 bytes still fail validation"                     "validate_pw \"\$(printf 'x%.0s' \$(seq 1 81))\""

summary
