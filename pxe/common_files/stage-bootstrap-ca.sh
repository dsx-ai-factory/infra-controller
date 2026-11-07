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

destination=$1

# Embedded trust is opt-in. Remove any artifact left by an earlier build when
# the current build does not provide a site-specific bundle.
if [ -z "${BOOTSTRAP_CA_PATH:-}" ]; then
    rm -f "$destination"
    exit 0
fi

source=$BOOTSTRAP_CA_PATH

if [ ! -s "$source" ]; then
    echo "bootstrap CA is missing or empty: $source" >&2
    exit 1
fi

if ! command -v openssl >/dev/null 2>&1; then
    echo "openssl is required to validate the bootstrap CA bundle" >&2
    exit 1
fi

# Check the PEM structure before asking OpenSSL to parse it. This rejects
# nested/misordered certificate blocks and other PEM material such as private
# keys, while allowing comments and CRLF-formatted bundles.
if ! awk '
    { sub(/\r$/, "") }
    /^[[:space:]]*-----BEGIN / {
        if ($0 != "-----BEGIN CERTIFICATE-----" || in_certificate) {
            exit 1
        }
        in_certificate = 1
        certificate_count++
        next
    }
    /^[[:space:]]*-----END / {
        if ($0 != "-----END CERTIFICATE-----" || !in_certificate) {
            exit 1
        }
        in_certificate = 0
        next
    }
    END {
        if (certificate_count == 0 || in_certificate) {
            exit 1
        }
    }
' "$source"; then
    echo "bootstrap CA is not a valid PEM certificate bundle: $source" >&2
    exit 1
fi

# crl2pkcs7 parses every certificate in a multi-certificate bundle. In
# contrast, `openssl x509 -in` validates only the first certificate.
if ! openssl crl2pkcs7 -nocrl -certfile "$source" -outform DER -out /dev/null; then
    echo "bootstrap CA contains an invalid certificate: $source" >&2
    exit 1
fi

destination_dir=$(dirname -- "$destination")
destination_name=$(basename -- "$destination")
temporary="$destination_dir/.${destination_name}.tmp.$$"
cleanup() {
    rm -f "$temporary"
}
trap cleanup EXIT HUP INT TERM

install -m 0644 "$source" "$temporary"
mv -f "$temporary" "$destination"
trap - EXIT HUP INT TERM
