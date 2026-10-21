-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Store operator-managed NIC firmware definitions independently of site configuration.
CREATE TABLE nic_firmware_profiles (
    id TEXT PRIMARY KEY,
    config JSONB NOT NULL,
    version VARCHAR(64) NOT NULL
);
