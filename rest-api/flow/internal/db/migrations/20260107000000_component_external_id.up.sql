-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Rename machine_id to external_id
ALTER TABLE component RENAME COLUMN machine_id TO external_id;

-- Drop old index and create new one
DROP INDEX IF EXISTS component_machine_id;
CREATE INDEX component_external_id_idx ON component (external_id);

-- Add unique index on (type, external_id)
CREATE UNIQUE INDEX component_type_external_id_idx ON component (type, external_id) WHERE external_id IS NOT NULL;

-- Drop unused associated_id column
ALTER TABLE component DROP COLUMN IF EXISTS associated_id;

