-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

--
-- Migration rollback: Remove async worker pool support
--

-- Drop the new index
DROP INDEX IF EXISTS firmware_update_last_checked_idx;

-- Restore the original active index (without CANCELLED)
DROP INDEX IF EXISTS firmware_update_active_idx;
CREATE INDEX firmware_update_active_idx 
    ON public.firmware_update (switch_uuid, component) 
    WHERE state NOT IN ('COMPLETED', 'FAILED');

-- Remove the new columns
ALTER TABLE public.firmware_update
    DROP COLUMN IF EXISTS last_checked_at;

ALTER TABLE public.firmware_update
    DROP COLUMN IF EXISTS exec_context;
