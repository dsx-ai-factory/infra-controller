-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

--
-- Migration: Update task table to rename component_ids to component_uuids
-- (1 task = 1 rack principle, rack_id already exists)
--

-- Add component_uuids column
ALTER TABLE public.task
    ADD COLUMN component_uuids jsonb;

-- Migrate existing data: copy component_ids to component_uuids
UPDATE public.task
SET component_uuids = component_ids
WHERE component_ids IS NOT NULL;

-- Drop old component_ids column
ALTER TABLE public.task
    DROP COLUMN component_ids;
