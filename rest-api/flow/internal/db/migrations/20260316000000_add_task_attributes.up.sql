-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Add flexible attributes column for task metadata. New fields can be added
-- to the Go TaskAttributes struct without further migrations.
-- component_uuids is dropped: all component targeting is now stored in
-- attributes.components_by_type with explicit type information.
ALTER TABLE task ADD COLUMN IF NOT EXISTS attributes jsonb NOT NULL DEFAULT '{}';
ALTER TABLE task DROP COLUMN IF EXISTS component_uuids;
