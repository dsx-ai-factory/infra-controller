#!/bin/bash
# Tests for --encrypt-artifacts (build-dpu-install-iso.sh) and the matching decryption in
# on-server/install.sh. Both function blocks are sourced verbatim from the shipped scripts:
#   - the cipher arguments are identical on both sides
#   - encrypt → decrypt round-trips a directory tree, files byte-identical, mode 600/700
#   - the SHA256SUMS manifest is present and verified
#   - wrong passphrase, tampered archive, missing passphrase → failure, no output left behind
#   - the plaintext never appears in the encrypted blob

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"

BUILD_SH="$UNIT_TEST_DIR/../build-dpu-install-iso.sh"
INSTALL_SH="$UNIT_TEST_DIR/../on-server/install.sh"

if ! command -v openssl >/dev/null 2>&1 || ! command -v shasum >/dev/null 2>&1; then
    echo "  SKIP: openssl and shasum are required"
    exit 0
fi

enc_block="$(sed -n '/^# >>> artifact-encryption functions/,/^# <<< artifact-encryption functions/p' "$BUILD_SH")"
dec_block="$(sed -n '/^# >>> artifact-decryption functions/,/^# <<< artifact-decryption functions/p' "$INSTALL_SH")"
eval "$enc_block"
ENC_ARGS="${ARTIFACT_CIPHER_ARGS[*]}"
eval "$dec_block"
DEC_ARGS="${ARTIFACT_CIPHER_ARGS[*]}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mode_of() { stat -c "%a" "$1" 2>/dev/null || stat -f "%OLp" "$1" 2>/dev/null; }

echo "=== both blocks sourced, cipher parameters agree ==="
assert_true "encrypt_artifacts_dir defined" "declare -F encrypt_artifacts_dir >/dev/null"
assert_true "decrypt_artifacts_to defined"  "declare -F decrypt_artifacts_to >/dev/null"
assert_eq   "cipher args identical in build and install scripts" "$ENC_ARGS" "$DEC_ARGS"
assert_true "PBKDF2 is used" "[[ '$ENC_ARGS' == *-pbkdf2* ]]"

echo ""
echo "=== round trip ==="
src="$WORK/servers"; mkdir -p "$src/node1" "$src/node2"
printf 'vrf:\n  bgp:\n    password: "s3cret-plaintext"\n' > "$src/node1/startup.yaml"
printf 'network: {version: 2}\n' > "$src/node1/99_config.yaml"
head -c 4096 /dev/urandom > "$src/node2/startup.yaml"
# On macOS an extended attribute makes tar emit an AppleDouble "._" entry unless suppressed.
xattr -w com.example.test 1 "$src/node1/startup.yaml" 2>/dev/null || true
chmod 644 "$src/node1/startup.yaml" "$src/node1/99_config.yaml"   # build-host modes must not leak into the target
if [[ "$(uname -s)" == Darwin ]]; then                              # Finder droppings must not reach the target
    : > "$src/node1/._startup.yaml"; : > "$src/.DS_Store"
fi
enc="$WORK/servers.tar.enc"
assert_true  "encrypt succeeds"          "DPU_ISO_ARTIFACT_PASSWORD='correct horse' encrypt_artifacts_dir '$src' '$enc' 2>/dev/null"
assert_true  "manifest written in source" "[ -f '$src/SHA256SUMS' ]"
assert_eq    "manifest lists the 3 files" "3" "$(wc -l < "$src/SHA256SUMS" | tr -d ' ')"
assert_false "plaintext absent from blob"  "grep -q 's3cret-plaintext' '$enc'"
dest="$WORK/out"
assert_true  "decrypt succeeds"           "DPU_ISO_ARTIFACT_PASSWORD='correct horse' decrypt_artifacts_to '$enc' '$dest' 2>/dev/null"
assert_true  "node1 startup.yaml identical" "cmp -s '$src/node1/startup.yaml' '$dest/node1/startup.yaml'"
assert_true  "node2 binary identical"       "cmp -s '$src/node2/startup.yaml' '$dest/node2/startup.yaml'"
assert_true  "netplan identical"            "cmp -s '$src/node1/99_config.yaml' '$dest/node1/99_config.yaml'"
assert_eq    "dest dir is mode 700"  "700" "$(mode_of "$dest")"
assert_eq    "extracted file is mode 600" "600" "$(mode_of "$dest/node1/startup.yaml")"
assert_eq    "no AppleDouble or .DS_Store entries extracted" "0" "$(find "$dest" -name '._*' -o -name '.DS_Store' | wc -l | tr -d ' ')"
# bsdtar hides AppleDouble members when listing or extracting, so inspect the raw
# archive with Python: this is what GNU tar on the site controller would see.
if command -v python3 >/dev/null 2>&1; then
    raw=$(DPU_ISO_ARTIFACT_PASSWORD='correct horse' openssl enc -d "${ARTIFACT_CIPHER_ARGS[@]}" -pass env:DPU_ISO_ARTIFACT_PASSWORD -in "$enc" 2>/dev/null \
        | python3 -c '
import sys,tarfile
ms=tarfile.open(fileobj=sys.stdin.buffer, mode="r|").getmembers()
apple=sum(1 for m in ms if "/._" in m.name or m.name.startswith("._") or m.name.endswith(".DS_Store"))
nonroot=sum(1 for m in ms if m.uid!=0 or m.gid!=0)
badmode=sum(1 for m in ms if (m.isdir() and (m.mode & 0o777)!=0o700) or (m.isfile() and (m.mode & 0o777)!=0o600))
print(apple, nonroot, badmode)')
    read -r raw_apple raw_nonroot raw_badmode <<< "$raw"
    assert_eq "raw archive carries no AppleDouble/.DS_Store members" "0" "$raw_apple"
    assert_eq "raw archive members are all uid 0 / gid 0"             "0" "$raw_nonroot"
    assert_eq "raw archive modes are 700 (dirs) / 600 (files)"        "0" "$raw_badmode"
fi
assert_eq    "manifest has no AppleDouble entries" "0" "$(grep -c '/\._' "$dest/SHA256SUMS")"

echo ""
echo "=== failure paths ==="
assert_false "wrong passphrase fails"     "DPU_ISO_ARTIFACT_PASSWORD='wrong' decrypt_artifacts_to '$enc' '$WORK/bad1' 2>/dev/null"
assert_false "no output on wrong passphrase" "[ -e '$WORK/bad1' ]"
assert_false "missing passphrase fails"   "DPU_ISO_ARTIFACT_PASSWORD= decrypt_artifacts_to '$enc' '$WORK/bad2' 2>/dev/null"
assert_false "no output on missing passphrase" "[ -e '$WORK/bad2' ]"
cp "$enc" "$WORK/tampered.enc"
printf '\x00' | dd of="$WORK/tampered.enc" bs=1 seek=100 conv=notrunc 2>/dev/null
assert_false "tampered archive fails"     "DPU_ISO_ARTIFACT_PASSWORD='correct horse' decrypt_artifacts_to '$WORK/tampered.enc' '$WORK/bad3' 2>/dev/null"
assert_false "no output on tampered archive" "[ -e '$WORK/bad3' ]"
assert_false "encrypt without passphrase fails" "DPU_ISO_ARTIFACT_PASSWORD= encrypt_artifacts_dir '$src' '$WORK/none.enc' 2>/dev/null"
assert_false "no blob written without passphrase" "[ -e '$WORK/none.enc' ]"

echo ""
echo "=== an existing tree survives a failed re-install and is replaced by a verified one ==="
keep="$WORK/keep"; mkdir -p "$keep/oldnode"; echo previous > "$keep/oldnode/startup.yaml"
assert_false "wrong passphrase over an existing tree fails" "DPU_ISO_ARTIFACT_PASSWORD='wrong' decrypt_artifacts_to '$enc' '$keep' 2>/dev/null"
assert_true  "existing tree untouched after the failure" "[ '$(cat "$keep/oldnode/startup.yaml")' = previous ]"
assert_false "tampered archive over an existing tree fails" "DPU_ISO_ARTIFACT_PASSWORD='correct horse' decrypt_artifacts_to '$WORK/tampered.enc' '$keep' 2>/dev/null"
assert_true  "existing tree still untouched" "[ -f '$keep/oldnode/startup.yaml' ]"
assert_eq    "no temporary directories left next to it" "0" "$(find "$WORK" -maxdepth 1 -name 'keep.new.*' | wc -l | tr -d ' ')"
assert_true  "correct passphrase replaces it" "DPU_ISO_ARTIFACT_PASSWORD='correct horse' decrypt_artifacts_to '$enc' '$keep' 2>/dev/null"
assert_false "old content gone after the verified replacement" "[ -e '$keep/oldnode' ]"
assert_true  "new content present" "cmp -s '$src/node1/startup.yaml' '$keep/node1/startup.yaml'"
assert_eq    "replaced tree is mode 700" "700" "$(mode_of "$keep")"

echo ""
echo "=== manifest catches a modified file (simulated tamper after decrypt) ==="
dest2="$WORK/out2"
DPU_ISO_ARTIFACT_PASSWORD='correct horse' decrypt_artifacts_to "$enc" "$dest2" 2>/dev/null
echo "changed" >> "$dest2/node1/startup.yaml"
assert_false "manifest check now fails" "(cd '$dest2' && shasum -a 256 --check --quiet --strict SHA256SUMS >/dev/null 2>&1)"

summary
