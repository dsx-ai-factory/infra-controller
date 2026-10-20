-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Hosts can select a machine-specific NIC firmware profile.
ALTER TABLE machines
    ADD COLUMN nic_firmware_profile_id TEXT
    CONSTRAINT machines_nic_firmware_profile_id_fkey
    REFERENCES nic_firmware_profiles(id);

CREATE INDEX machines_nic_firmware_profile_id_idx
    ON machines (nic_firmware_profile_id);
