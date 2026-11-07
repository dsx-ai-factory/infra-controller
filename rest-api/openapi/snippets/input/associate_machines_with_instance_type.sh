# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X POST "https://api.example.com/v2/org/{provider-org-name}/nico/instance/type/9c4aaa6a-3934-4274-b0a9-5143b253039e/machine" \
-H "Content-Type: application/json" -H "Accept: application/json" \
-H "Authorization: Bearer ${TOKEN}" \
-d '{
      "machineIds": [
         "fm100hthvos96dbmmai84gsok0dn967v9fap8ublgp34kaknd9tq7pddim0",
         "fm100httuapjc6t4o629o5d3uu5616gimvn0smunp199mmmp1f2134nt92g"
      ]
      }'
