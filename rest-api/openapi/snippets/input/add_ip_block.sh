# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X POST "https://api.example.com/v2/org/{provider-org-name}/nico/ipblock" \
  -H "Content-Type: application/json" -H "Accept: application/json" \
  -H "Authorization: Bearer ${TOKEN}" \
  -d '{
        "name": "demo-ipv4-block",
        "description": "Demo IPv4 block",
        "prefixLength": 24,
        "prefix": "192.166.128.0",
        "protocolVersion": "IPv4",
        "routingType": "DatacenterOnly",
        "siteId": "bd4692bd-da95-410e-911a-d492fe2d35f8"
      }'
