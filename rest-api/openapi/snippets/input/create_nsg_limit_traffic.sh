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
          "name": "string",
          "direction": "INGRESS",
          "sourcePortRange": "80-81",
          "destinationPortRange": "80-81",
          "protocol": "TCP",
          "action": "PERMIT",
          "priority": 0,
          "sourcePrefix": "10.5.44.0/24",
          "destinationPrefix": "10.5.44.0/24"
        }
      ],
    "labels": {}
  }'
