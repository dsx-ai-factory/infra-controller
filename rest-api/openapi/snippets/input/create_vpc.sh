# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X POST "https://api.example.com/v2/org/{tenant-org-name}/nico/vpc" \
-H "Content-Type: application/json" -H "Accept: application/json" \
-H "Authorization: Bearer ${TOKEN}" \
-d '{
   "name": "demo-vpc",
   "siteId": "bd4692bd-da95-410e-911a-d492fe2d35f8",
   "description": "Demo tenant VPC",
   "networkVirtualizationType": "FNN"
   }'
