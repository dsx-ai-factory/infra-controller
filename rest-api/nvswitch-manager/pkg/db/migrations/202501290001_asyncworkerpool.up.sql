-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

--
-- Migration: Add async worker pool support
-- Adds exec_context and last_checked_at for non-blocking firmware update workers
--

-- Add exec_context JSONB column for persisting async execution state
-- This stores TaskURI (Redfish), PID (Script/SSH), reachability tracking, etc.
ALTER TABLE public.firmware_update
    ADD COLUMN exec_context JSONB;

-- Add last_checked_at timestamp for poll timing
-- Workers use this to determine when an active update needs re-polling
ALTER TABLE public.firmware_update
    ADD COLUMN last_checked_at TIMESTAMP WITH TIME ZONE;

-- Index on last_checked_at for efficient polling queries
-- The ClaimNextWorkItem query orders by last_checked_at ASC NULLS FIRST
-- to prioritize updates that haven't been checked recently
CREATE INDEX firmware_update_last_checked_idx 
    ON public.firmware_update (last_checked_at ASC NULLS FIRST)
    WHERE state NOT IN ('QUEUED', 'COMPLETED', 'FAILED', 'CANCELLED');

-- Update the active index to include CANCELLED state
DROP INDEX IF EXISTS firmware_update_active_idx;
CREATE INDEX firmware_update_active_idx 
    ON public.firmware_update (switch_uuid, component) 
    WHERE state NOT IN ('COMPLETED', 'FAILED', 'CANCELLED');
