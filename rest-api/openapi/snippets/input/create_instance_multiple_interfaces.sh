# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X POST "https://api.example.com/v2/org/{tenant-org-name}/nico/instance" \
   -H "Content-Type: application/json" -H "Accept: application/json" \
   -H "Authorization: Bearer ${TOKEN}" \
   -d '{
         "name": "demo-compute-instance-multiple-interfaces",
         "instanceTypeId": "364dd639-5122-420c-a663-fa56e290e187",
         "vpcId": "0b1c53a0-a27e-4714-98d7-0cd3bc579db2",
         "tenantId": "aaf3cb83-8785-4265-a3bd-61e828f87db8",
         "operatingSystemId": "0865029e-3979-432d-985e-2de396ecce32",
         "userData": null,
         "interfaces": [
            {
               "vpcPrefixId": "8c7422d7-abf5-41ae-8b6d-9d62442a8b31",
               "isPhysical": true,
               "device": "MT43244 BlueField-3 integrated ConnectX-7 network controller",
               "deviceInstance": 0
            },
            {
               "vpcPrefixId": "8988dbd3-f038-4338-b961-8e5cbf89a77e",
               "isPhysical": true,
               "device": "MT43244 BlueField-3 integrated ConnectX-7 network controller",
               "deviceInstance": 1
            }
         ]
         }'
