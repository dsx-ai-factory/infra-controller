#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

namespace="${1:-nico-system}"

# Kea distinguishes floating-point tokens from integers. Older jq versions
# serialize 0.0 as 0, which makes cache-threshold invalid. Preserve that type
# both when rewriting the configuration and when comparing/checksumming it.
canonical_config() {
  python3 -c 'import json, sys; print(json.dumps(json.load(sys.stdin), sort_keys=True, separators=(",", ":")))'
}

# Helm creates the Services first. Kea requires literal IPv4 addresses, not DNS
# names, so fill only the chart placeholders after Kubernetes assigns the IPs.
service_ipv4() {
  kubectl -n "${namespace}" get service "$1" -o json |
    jq -er '.spec.clusterIPs[] | select(contains(":") | not) | select(. != "None")'
}
dns_ip="$(service_ipv4 nico-dns)"
ntp_ip="$(service_ipv4 nico-ntp-client)"
pxe_ip="$(service_ipv4 nico-pxe)"
config="$(kubectl -n "${namespace}" get configmap nico-dhcp-config -o json)"
original="$(jq -er '.data["kea_config.json"]' <<<"${config}")"
updated="$(python3 -c '
import json
import sys

config = json.load(sys.stdin)
replacements = dict(zip(
    ("REPLACE_WITH_NICO_DNS_VIP", "REPLACE_WITH_NICO_NTP_VIPS", "REPLACE_WITH_NICO_PXE_VIP"),
    sys.argv[1:],
))
for hook in config["Dhcp4"]["hooks-libraries"]:
    parameters = hook.get("parameters")
    if parameters:
        for key, value in parameters.items():
            if isinstance(value, str):
                parameters[key] = replacements.get(value, value)
print(json.dumps(config, indent=2))
' "${dns_ip}" "${ntp_ip}" "${pxe_ip}" <<<"${original}")"
if [[ "$(canonical_config <<<"${original}")" != "$(canonical_config <<<"${updated}")" ]]; then
  patch="$(jq -n --arg config "${updated}" '{data: {"kea_config.json": $config}}')"
  kubectl -n "${namespace}" patch configmap nico-dhcp-config --type=merge -p "${patch}"
fi

# Reconcile independently of the ConfigMap patch so a failed rollout update can
# be retried. An unchanged checksum leaves the Deployment pod template unchanged.
checksum="$(canonical_config <<<"${updated}" | shasum -a 256 | awk '{print $1}')"
patch="$(jq -n --arg checksum "${checksum}" '
  {spec: {template: {metadata: {annotations: {
    "devspace.nvidia.com/dhcp-config-checksum": $checksum
  }}}}}')"
kubectl -n "${namespace}" patch deployment nico-dhcp --type=merge -p "${patch}"
