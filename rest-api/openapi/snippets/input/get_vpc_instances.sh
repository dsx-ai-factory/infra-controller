# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X GET "https://api.example.com/v2/org/{tenant-org-name}/nico/instance?vpcId=f466a2d5-5820-4824-a845-3218fdff801b" \
-H "Content-Type: application/json" -H "Accept: application/json" \
-H "Authorization: Bearer ${TOKEN}"
