-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Add rack_id column to nvswitch table
-- This allows associating NV-Switch trays with physical racks for filtering and batch operations

ALTER TABLE nvswitch ADD COLUMN rack_id VARCHAR(64);

-- Index for efficient filtering by rack
CREATE INDEX nvswitch_rack_id_idx ON nvswitch(rack_id) WHERE rack_id IS NOT NULL;
