# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X GET "https://api.example.com/v2/org/{provider-org-name}/nico/ipblock?siteId=157627d6-d742-440b-ac04-77a618d94459" \
  -H "Content-Type: application/json" -H "Accept: application/json" \
  -H "Authorization: Bearer ${TOKEN}"
