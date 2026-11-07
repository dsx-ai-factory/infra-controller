# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X POST "https://api.example.com/v2/org/{provider-org-name}/nico/allocation" \
  -H "Content-Type: application/json" -H "Accept: application/json" \
  -H "Authorization: Bearer ${TOKEN}" \
  -d '{
        "name": "demo-compute-allocation",
        "description": "Demo compute instance allocation",
        "siteId": "bd4692bd-da95-410e-911a-d492fe2d35f8",
        "tenantId": "aaf3cb83-8785-4265-a3bd-61e828f87db8",
        "allocationConstraints": [
          {
            "resourceType": "InstanceType",
            "resourceTypeId": "9c4aaa6a-3934-4274-b0a9-5143b253039e",
            "constraintType": "Reserved",
            "constraintValue": 2
          }
        ]
      }'
