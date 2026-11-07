#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -uo pipefail

BMC_IP="${1:-}"
USER="${2:-}"
PASS="${3:-}"

if [[ -z "$BMC_IP" || -z "$USER" || -z "$PASS" ]]; then
  echo "Usage: $0 <BMC_IP> <USER> <PASSWORD>"
  exit 1
fi

############################################
# Trigger Redfish PowerCycle and verify
############################################

echo "Issuing Redfish PowerCycle to ${BMC_IP}..."

resp_json="$(mktemp)"
# IPv6 hosts need brackets in URLs; ping uses the original address.
URL_HOST="$BMC_IP"
if [[ "$URL_HOST" == *:* && "$URL_HOST" != \[*\] ]]; then
  URL_HOST="[$URL_HOST]"
fi

if ! curl -ksu "${USER}:${PASS}" \
      -H "Content-Type: application/json" \
      -X POST "https://${URL_HOST}/redfish/v1/Systems/System_0/Actions/ComputerSystem.Reset" \
      -d '{"ResetType": "PowerCycle"}' \
      -o "$resp_json"; then
  echo "Redfish PowerCycle POST failed (curl error)"
  rm -f "$resp_json"
  exit 1
fi

# Example expected JSON:
# {
#   "@Message.ExtendedInfo": [
#     {
#       "Message": "The request completed successfully.",
#       "MessageId": "Base.1.18.1.Success",
#       "MessageSeverity": "OK"
#     }
#   ]
# }

severity="$(grep -oE '"MessageSeverity"\s*:\s*"[^"]+"' "$resp_json" \
            | head -n1 \
            | sed -E 's/.*"MessageSeverity"\s*:\s*"([^"]*)".*/\1/')"

msgid="$(grep -oE '"MessageId"\s*:\s*"[^"]+"' "$resp_json" \
         | head -n1 \
         | sed -E 's/.*"MessageId"\s*:\s*"([^"]*)".*/\1/')"

rm -f "$resp_json"

if [[ "$severity" != "OK" ]]; then
  echo "PowerCycle request did not return OK (MessageSeverity='${severity}', MessageId='${msgid}')"
  exit 1
fi

echo "PowerCycle request accepted (MessageSeverity=${severity}, MessageId=${msgid})."

############################################
# Wait for host to go down, then come back
############################################

echo -n "Waiting for ${BMC_IP} to go down"
while ping -W 1 -c 1 "$BMC_IP" &>/dev/null; do
  echo -n "."
  sleep 2
done
echo

echo -n "Waiting for ${BMC_IP} to come back up"
while ! ping -W 1 -c 1 "$BMC_IP" &>/dev/null; do
  echo -n "."
  sleep 2
done
echo

echo "Host ${BMC_IP} is back online."
