-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Add foreign key with ON DELETE CASCADE so BMC rows are automatically removed
-- when their parent component is hard-deleted (purged).
ALTER TABLE bmc
    ADD CONSTRAINT fk_bmc_component
    FOREIGN KEY (component_id) REFERENCES component(id) ON DELETE CASCADE;
