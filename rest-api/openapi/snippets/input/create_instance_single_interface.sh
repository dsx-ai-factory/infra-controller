# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X POST "https://api.example.com/v2/org/{tenant-org-name}/nico/instance" \
   -H "Content-Type: application/json" -H "Accept: application/json" \
   -H "Authorization: Bearer ${TOKEN}" \
   -d '{
      "name": "demo-compute-instance-0",
      "instanceTypeId": "9c4aaa6a-3934-4274-b0a9-5143b253039e",
      "vpcId": "f466a2d5-5820-4824-a845-3218fdff801b",
      "tenantId": "aaf3cb83-8785-4265-a3bd-61e828f87db8",
      "operatingSystemId": "0865029e-3979-432d-985e-2de396ecce32",
      "userData": null,
      "interfaces": [
         {
            "subnetId": "5e1f6c51-a532-437b-b7a5-7dfac214de08"
         }
      ]
      }'
