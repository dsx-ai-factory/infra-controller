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
stage_script="$script_dir/stage-bootstrap-ca.sh"
valid_ca="$repo_root/dev/forge_prodroot.pem"
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

expect_success() {
    name=$1
    source=$2
    destination="$temp_dir/$name-output.pem"

    BOOTSTRAP_CA_PATH=$source sh "$stage_script" "$destination"
    cmp -s "$source" "$destination" || fail "$name: staged bytes differ"
    [ "$(file_mode "$destination")" = 644 ] || fail "$name: mode is not 0644"
    if find "$(dirname "$destination")" -name ".$(basename "$destination").tmp.*" | grep -q .; then
        fail "$name: temporary staging file was left behind"
    fi
}

expect_failure() {
    name=$1
    source=$2
    destination="$temp_dir/$name-output.pem"
    cp "$valid_ca" "$destination"

    if BOOTSTRAP_CA_PATH=$source sh "$stage_script" "$destination" >/dev/null 2>&1; then
        fail "$name: invalid bundle was accepted"
    fi
    cmp -s "$valid_ca" "$destination" || fail "$name: existing destination changed"
}

multi_ca="$temp_dir/multi.pem"
cp "$valid_ca" "$multi_ca"
cat "$valid_ca" >>"$multi_ca"

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

truncated_ca="$temp_dir/truncated.pem"
printf '%s\n' '-----BEGIN CERTIFICATE-----' 'not-finished' >"$truncated_ca"

private_key_ca="$temp_dir/private-key.pem"
cp "$valid_ca" "$private_key_ca"
printf '%s\n' \
    '-----BEGIN PRIVATE KEY-----' \
    'bm90LWEtcHJpdmF0ZS1rZXk=' \
    '-----END PRIVATE KEY-----' >>"$private_key_ca"

indented_private_key_ca="$temp_dir/indented-private-key.pem"
cp "$valid_ca" "$indented_private_key_ca"
printf '%s\n' \
    '    -----BEGIN PRIVATE KEY-----' \
    '    bm90LWEtcHJpdmF0ZS1rZXk=' \
    '    -----END PRIVATE KEY-----' >>"$indented_private_key_ca"

expect_success single "$valid_ca"
expect_success multi "$multi_ca"
expect_success crlf "$crlf_ca"

replacement_destination="$temp_dir/replacement-output.pem"
cp "$valid_ca" "$replacement_destination"
BOOTSTRAP_CA_PATH=$multi_ca sh "$stage_script" "$replacement_destination"
cmp -s "$multi_ca" "$replacement_destination" || fail "replacement: staged bytes differ"
[ "$(file_mode "$replacement_destination")" = 644 ] || fail "replacement: mode is not 0644"

expect_failure malformed "$malformed_ca"
expect_failure malformed-second "$malformed_second_ca"
expect_failure truncated "$truncated_ca"
expect_failure private-key "$private_key_ca"
expect_failure indented-private-key "$indented_private_key_ca"

stale_destination="$temp_dir/stale.pem"
cp "$valid_ca" "$stale_destination"
unset BOOTSTRAP_CA_PATH
sh "$stage_script" "$stale_destination"
[ ! -e "$stale_destination" ] || fail "unset input did not remove a stale embedded bundle"
