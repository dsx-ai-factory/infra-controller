# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X POST "https://api.example.com/v2/org/{tenant-org-name}/nico/operating-system" \
  -H "Content-Type: application/json" -H "Accept: application/json" \
  -H "Authorization: Bearer ${TOKEN}" \
  -d '{
        "name": "ubuntu-22.04-image",
        "description": "Ubuntu 22.04 image-based installation",
        "tenantId": "7306ff7d-f2b4-472f-ba1c-3ec9c24967be",
        "siteIds": ["497f6eca-6276-4993-bfeb-53cbbbba6f08"],
        "imageUrl": "https://cloud-images.ubuntu.com/releases/jammy/release-20260826/ubuntu-22.04-server-cloudimg-amd64.img",
        "imageSha": "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
        "imageDisk": "/dev/disk/by-id/nvme-Dell_BOSS-N1_VNOWW12345V0123451UT",
        "rootFsLabel": "cloudimg-rootfs",
        "phoneHomeEnabled": false,
        "allowOverride": false
      }'
