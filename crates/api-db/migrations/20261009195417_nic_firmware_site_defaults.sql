-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Select a live NIC firmware profile for each managed hardware pair.
CREATE TABLE nic_firmware_site_defaults (
    part_number TEXT NOT NULL,
    psid TEXT NOT NULL,
    profile_id TEXT NOT NULL REFERENCES nic_firmware_profiles(id),
    version VARCHAR(64) NOT NULL,
    PRIMARY KEY (part_number, psid)
);

CREATE INDEX nic_firmware_site_defaults_profile_id_idx
    ON nic_firmware_site_defaults (profile_id);
