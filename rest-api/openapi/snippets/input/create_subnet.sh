# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X POST "https://api.example.com/v2/org/{tenant-org-name}/nico/subnet" \
-H "Content-Type: application/json" -H "Accept: application/json" \
-H "Authorization: Bearer ${TOKEN}" \
-d '{
        "name": "demo-ipv4-subnet",
        "description": "Demo IPv4 Tenant Subnet",
        "vpcId": "f466a2d5-5820-4824-a845-3218fdff801b",
        "ipv4BlockId": "20d7dd4f-ae43-4245-a9d9-d093296009c4",
        "prefixLength": 28
    }'
