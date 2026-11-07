# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X PATCH "https://api.example.com/v2/org/{tenant-org-name}/nico/sshkeygroup/9ffb8f90-f88f-4420-952d-e911f446d7eb" \
-H "Content-Type: application/json" -H "Accept: application/json" \
-H "Authorization: Bearer ${TOKEN}" \
-d '{
      "version": "23220d11a579258cb810942060e910fb3fac9762",
      "siteIds": [
         "157627d6-d742-440b-ac04-77a618d94459"
      ]
      }'
