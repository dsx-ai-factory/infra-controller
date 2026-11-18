-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Keep card selections with their host while profiles remain live references.
CREATE TABLE machine_nic_firmware_overrides (
    machine_id VARCHAR(64) NOT NULL
        REFERENCES machines(id) ON UPDATE CASCADE ON DELETE CASCADE,
    selector JSONB NOT NULL,
    profile_id TEXT NOT NULL
        CONSTRAINT machine_nic_firmware_overrides_profile_id_fkey
        REFERENCES nic_firmware_profiles(id),
    PRIMARY KEY (machine_id, selector)
);

CREATE INDEX machine_nic_firmware_overrides_profile_id_idx
    ON machine_nic_firmware_overrides (profile_id);
