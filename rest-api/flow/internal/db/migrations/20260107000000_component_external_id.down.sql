-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Restore associated_id column
ALTER TABLE component ADD COLUMN IF NOT EXISTS associated_id character varying;

-- Drop unique index on (type, external_id)
DROP INDEX IF EXISTS component_type_external_id_idx;

-- Drop new index
DROP INDEX IF EXISTS component_external_id_idx;

-- Rename external_id back to machine_id
ALTER TABLE component RENAME COLUMN external_id TO machine_id;

-- Recreate old index
CREATE INDEX component_machine_id ON component (machine_id);
