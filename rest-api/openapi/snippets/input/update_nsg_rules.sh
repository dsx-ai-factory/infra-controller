# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X PATCH "https://api.example.com/v2/org/{org}/nico/network-security-group/{nsgId}" \
  -H "Content-Type: application/json" -H "Accept: application/json" \
  -H "Authorization: Bearer ${TOKEN}" \
  -d '{
    "rules": [
      {
        "name": "allow-all-ingress",
        "direction": "INGRESS",
        "sourcePortRange": "0-65535",
        "destinationPortRange": "0-65535",
        "protocol": "ANY",
        "action": "PERMIT",
        "priority": 0,
        "sourcePrefix": "192.168.1.0/24",
        "destinationPrefix": "0.0.0.0/0"
      }
    ]
  }'
