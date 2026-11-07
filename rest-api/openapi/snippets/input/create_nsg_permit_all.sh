# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X POST "https://api.example.com/v2/org/{org}/nico/network-security-group" \
  -H "Content-Type: application/json" -H "Accept: application/json" \
  -H "Authorization: Bearer ${TOKEN}" \
  -d '{
    "name": "string",
    "description": "string",
    "siteId": "60189e9c-7d12-438c-b9ca-6998d9c364b1",
    "rules": [
      {
        "name": "allow-all-ingress",
        "direction": "INGRESS",
        "sourcePortRange": "0-65535",
        "destinationPortRange": "0-65535",
        "protocol": "ANY",
        "action": "PERMIT",
        "priority": 0,
        "sourcePrefix": "0.0.0.0/0",
        "destinationPrefix": "0.0.0.0/0"
      },
      {
        "name": "allow-all-egress",
        "direction": "EGRESS",
        "sourcePortRange": "0-65535",
        "destinationPortRange": "0-65535",
        "protocol": "ANY",
        "action": "PERMIT",
        "priority": 0,
        "sourcePrefix": "0.0.0.0/0",
        "destinationPrefix": "0.0.0.0/0"
      }
    ],
    "labels": {
      "property1": "default",
      "property2": "global-allow"
    }
    }'
