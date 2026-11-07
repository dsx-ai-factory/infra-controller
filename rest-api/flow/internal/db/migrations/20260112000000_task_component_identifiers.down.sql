-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

--
-- Rollback migration: Revert task table changes
--

-- Add back old component_ids column
ALTER TABLE public.task
    ADD COLUMN component_ids jsonb;

-- Migrate data back: rename component_uuids to component_ids
UPDATE public.task
SET component_ids = component_uuids;

-- Drop component_uuids column
ALTER TABLE public.task
    DROP COLUMN component_uuids;
