# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X PATCH "https://api.example.com/v2/org/{provider-org-name}/nico/site/2ae25bd4-7b07-4b39-9514-031e5c335f4f" \
  -H "Content-Type: application/json" -H "Accept: application/json" \
  -H "Authorization: Bearer ${TOKEN}" \
  -d '{
        "serialConsoleHostname": "console.example.com"
      }'
