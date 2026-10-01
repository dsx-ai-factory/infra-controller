#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

namespace="${1:-nico-system}"

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
updated="$(jq --arg dns "${dns_ip}" --arg ntp "${ntp_ip}" --arg pxe "${pxe_ip}" '
  .Dhcp4["hooks-libraries"] |= map(
    if .parameters then .parameters |= with_entries(
      if .value == "REPLACE_WITH_NICO_DNS_VIP" then .value = $dns
      elif .value == "REPLACE_WITH_NICO_NTP_VIPS" then .value = $ntp
      elif .value == "REPLACE_WITH_NICO_PXE_VIP" then .value = $pxe
      else . end)
    else . end)' <<<"${original}")"
if [[ "$(jq -cS . <<<"${original}")" != "$(jq -cS . <<<"${updated}")" ]]; then
  patch="$(jq -n --arg config "${updated}" '{data: {"kea_config.json": $config}}')"
  kubectl -n "${namespace}" patch configmap nico-dhcp-config --type=merge -p "${patch}"
fi

# Reconcile independently of the ConfigMap patch so a failed rollout update can
# be retried. An unchanged checksum leaves the Deployment pod template unchanged.
checksum="$(jq -cS . <<<"${updated}" | shasum -a 256 | awk '{print $1}')"
patch="$(jq -n --arg checksum "${checksum}" '
  {spec: {template: {metadata: {annotations: {
    "devspace.nvidia.com/dhcp-config-checksum": $checksum
  }}}}}')"
kubectl -n "${namespace}" patch deployment nico-dhcp --type=merge -p "${patch}"
