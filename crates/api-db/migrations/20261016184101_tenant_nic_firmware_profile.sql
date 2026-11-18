-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Tenant firmware selections retain their catalog profile until cleared.
ALTER TABLE tenants
    ADD COLUMN nic_firmware_profile_id TEXT
    CONSTRAINT tenants_nic_firmware_profile_id_fkey
    REFERENCES nic_firmware_profiles(id);

CREATE INDEX tenants_nic_firmware_profile_id_idx
    ON tenants (nic_firmware_profile_id);
