#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -eu

fail() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

TARGET_VERSION="${TARGET_VERSION:-}"
COMPONENT_TYPE="${COMPONENT_TYPE:-unknown}"

firmware_file="${1:-}"

[ -n "$firmware_file" ] || fail "Scout did not pass a firmware file"
[ -f "$firmware_file" ] || fail "Firmware image does not exist: $firmware_file"
command -v mlxfwmanager >/dev/null 2>&1 || fail "mlxfwmanager is required"
command -v awk >/dev/null 2>&1 || fail "awk is required"

printf 'Scout component=%s target_version=%s\n' "$COMPONENT_TYPE" "${TARGET_VERSION:-unset}"

# mlxfwmanager may exit non-zero when unrelated devices fail to open, while
# still printing valid XML for the devices this script needs to handle.
query_xml="$(
    mlxfwmanager --query --query-format XML -i "$firmware_file" || true
)"

# Treat the query as usable only if it produced the expected XML envelope.
case "$query_xml" in
    *"<Devices>"*"</Devices>"*)
        ;;
    *)
        fail "mlxfwmanager query failed: ${query_xml:-no output from mlxfwmanager}"
        ;;
esac

# Select only ConnectX-7 devices for which this firmware file provides an
# available FW image. Devices with available="N/A" are not suitable targets.
devices="$(
    printf '%s\n' "$query_xml" |
        awk '
            function attr(s, name,    pat, val) {
                pat = name "=\"[^\"]*\""
                if (match(s, pat)) {
                    val = substr(s, RSTART, RLENGTH)
                    sub(name "=\"", "", val)
                    sub(/"$/, "", val)
                    return val
                }
                return ""
            }

            /<Device[ >]/ {
                in_device = 1
                pci = attr($0, "pciName")
                type = attr($0, "type")
                is_cx7 = (type ~ /^ConnectX-?7/)
                has_available_fw = 0
            }

            in_device && /<FW[ >]/ {
                available = attr($0, "available")
                if (available != "" && available != "N/A") {
                    has_available_fw = 1
                }
            }

            /<\/Device>/ {
                if (is_cx7 && has_available_fw && pci != "") {
                    if (out != "") out = out ";"
                    out = out pci
                }
                in_device = 0
            }

            END {
                print out
            }
        '
)"

# This script is expected to run on hosts with eligible ConnectX-7 devices, so
# no matching device means the firmware file or discovered inventory is wrong.
if [ -z "$devices" ]; then
    fail "No ConnectX-7 devices are eligible for firmware file: $firmware_file"
fi

printf 'Running mlxfwmanager -u -f -i %s --skip_if_same -d %s -y\n' "$firmware_file" "$devices"

# --skip_if_same makes already-flashed devices a successful no-op.
mlxfwmanager -u -f -i "$firmware_file" --skip_if_same -d "$devices" -y

printf 'mlxfwmanager update has finished\n'
