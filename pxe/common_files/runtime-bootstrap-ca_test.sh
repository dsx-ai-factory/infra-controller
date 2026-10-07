#!/usr/bin/env sh
#
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
#
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
template="$repo_root/pxe/templates/user-data"
valid_ca="$repo_root/dev/forge_prodroot.pem"
private_key="$repo_root/dev/certs/server_identity.key"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT

fail() {
    echo "$1" >&2
    exit 1
}

file_mode() {
    if mode=$(stat -c '%a' "$1" 2>/dev/null); then
        printf '%s\n' "$mode"
    else
        stat -f '%Lp' "$1"
    fi
}

# Exercise the exact functions emitted into cloud-init instead of maintaining
# a test-only copy of the validation rules.
helpers="$temp_dir/bootstrap-ca-helpers.sh"
awk '
    /^      # BEGIN bootstrap CA validation helpers$/ {
        found_start = 1
        capture = 1
        next
    }
    /^      # END bootstrap CA validation helpers$/ {
        found_end = 1
        capture = 0
        next
    }
    capture {
        sub(/^      /, "")
        print
    }
    END {
        if (!found_start || !found_end) {
            exit 1
        }
    }
' "$template" >"$helpers" || fail "could not extract bootstrap CA helpers"
printf '\n"$@"\n' >>"$helpers"

multi_ca="$temp_dir/multi.pem"
cp "$valid_ca" "$multi_ca"
cat "$valid_ca" >>"$multi_ca"

empty_ca="$temp_dir/empty.pem"
: >"$empty_ca"

crlf_ca="$temp_dir/crlf.pem"
awk '{ printf "%s\r\n", $0 }' "$valid_ca" >"$crlf_ca"

malformed_ca="$temp_dir/malformed.pem"
printf '%s\n' \
    '-----BEGIN CERTIFICATE-----' \
    'not-a-certificate' \
    '-----END CERTIFICATE-----' >"$malformed_ca"

malformed_second_ca="$temp_dir/malformed-second.pem"
cp "$valid_ca" "$malformed_second_ca"
cat "$malformed_ca" >>"$malformed_second_ca"

certificate_and_key="$temp_dir/certificate-and-key.pem"
cp "$valid_ca" "$certificate_and_key"
cat "$private_key" >>"$certificate_and_key"

certificate_and_indented_key="$temp_dir/certificate-and-indented-key.pem"
cp "$valid_ca" "$certificate_and_indented_key"
sed 's/^/    /' "$private_key" >>"$certificate_and_indented_key"

expect_embedded_success() {
    name=$1
    source=$2
    destination="$temp_dir/$name-embedded.pem"

    bash "$helpers" install_embedded_bootstrap_ca "$source" "$destination"
    cmp -s "$source" "$destination" || fail "$name: embedded bytes differ"
    [ "$(file_mode "$destination")" = 644 ] || fail "$name: embedded mode is not 0644"
}

expect_embedded_replacement_success() {
    destination="$temp_dir/replaced-embedded.pem"
    cp "$valid_ca" "$destination"
    chmod 0600 "$destination"

    bash "$helpers" install_embedded_bootstrap_ca "$multi_ca" "$destination"
    cmp -s "$multi_ca" "$destination" || fail "embedded replacement bytes differ"
    [ "$(file_mode "$destination")" = 644 ] || fail "embedded replacement mode is not 0644"
}

expect_mounted_success() {
    name=$1
    source=$2

    bash "$helpers" accept_mounted_bootstrap_ca "$source" || fail "$name: mounted bundle was rejected"
}

expect_embedded_failure() {
    name=$1
    source=$2
    destination="$temp_dir/$name-embedded.pem"
    cp "$valid_ca" "$destination"

    if bash "$helpers" install_embedded_bootstrap_ca "$source" "$destination" >/dev/null 2>&1; then
        fail "$name: invalid embedded bundle was accepted"
    fi
    cmp -s "$valid_ca" "$destination" || fail "$name: invalid embedded bundle changed destination"
}

expect_embedded_install_failure() {
    destination="$temp_dir/install-failure-embedded.pem"
    failing_bin="$temp_dir/failing-install-bin"
    mkdir -p "$failing_bin"
    printf '%s\n' \
        '#!/usr/bin/env sh' \
        'printf "%s\n" partial >"$4"' \
        'exit 1' >"$failing_bin/install"
    chmod +x "$failing_bin/install"
    cp "$valid_ca" "$destination"

    if PATH="$failing_bin:$PATH" bash "$helpers" install_embedded_bootstrap_ca "$multi_ca" "$destination" >/dev/null 2>&1; then
        fail "partial embedded install unexpectedly succeeded"
    fi
    cmp -s "$valid_ca" "$destination" || fail "partial embedded install changed destination"
    for temporary in "$destination".tmp.*; do
        [ ! -e "$temporary" ] || fail "partial embedded install left temporary file"
    done
}

expect_mounted_failure() {
    name=$1
    source=$2

    if bash "$helpers" accept_mounted_bootstrap_ca "$source" >/dev/null 2>&1; then
        fail "$name: invalid mounted bundle was accepted"
    fi
}

expect_embedded_success single "$valid_ca"
expect_embedded_success multi "$multi_ca"
expect_embedded_success crlf "$crlf_ca"
expect_embedded_replacement_success
expect_mounted_success single "$valid_ca"
expect_mounted_success multi "$multi_ca"
expect_mounted_success crlf "$crlf_ca"

expect_embedded_failure malformed "$malformed_ca"
expect_embedded_failure malformed-second "$malformed_second_ca"
expect_embedded_failure certificate-and-key "$certificate_and_key"
expect_embedded_failure certificate-and-indented-key "$certificate_and_indented_key"
expect_embedded_failure empty "$empty_ca"
expect_embedded_install_failure
expect_mounted_failure malformed "$malformed_ca"
expect_mounted_failure malformed-second "$malformed_second_ca"
expect_mounted_failure certificate-and-key "$certificate_and_key"
expect_mounted_failure certificate-and-indented-key "$certificate_and_indented_key"
expect_mounted_failure empty "$empty_ca"
