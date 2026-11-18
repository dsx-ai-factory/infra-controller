# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X POST "https://api.example.com/v2/org/{tenant-org-name}/nico/vpc-prefix" \
  -H "Content-Type: application/json" -H "Accept: application/json" \
  -H "Authorization: Bearer ${TOKEN}" \
  -d '{
        "name": "application-network",
        "vpcId": "0b1c53a0-a27e-4714-98d7-0cd3bc579db2",
        "ipBlockId": "20d7dd4f-ae43-4245-a9d9-d093296009c4",
        "prefix": "10.217.98.0/25"
      }'
