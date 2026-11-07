# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X PATCH "https://api.example.com/v2/org/{tenant-org-name}/nico/sshkeygroup/9ffb8f90-f88f-4420-952d-e911f446d7eb" \
-H "Content-Type: application/json" -H "Accept: application/json" \
-H "Authorization: Bearer ${TOKEN}" \
-d '{
      "version": "6a5ccd83b5daf693bd14ab32a439d3181635be6f",
      "sshKeyIds": [
        "b658db7e-f06c-4140-9494-48ea1f3f7769"
      ]
    }'
