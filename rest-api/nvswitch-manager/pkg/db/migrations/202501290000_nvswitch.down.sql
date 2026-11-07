-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Drop firmware_update table (must drop first due to FK constraint)
DROP INDEX IF EXISTS public.firmware_update_predecessor_idx;
DROP INDEX IF EXISTS public.firmware_update_bundle_idx;
DROP INDEX IF EXISTS public.firmware_update_active_idx;
DROP INDEX IF EXISTS public.firmware_update_switch_uuid_idx;
DROP INDEX IF EXISTS public.firmware_update_state_created_idx;
DROP INDEX IF EXISTS public.firmware_update_created_at_idx;
DROP INDEX IF EXISTS public.firmware_update_state_idx;
DROP TABLE IF EXISTS public.firmware_update;

-- Drop nvswitch table
DROP INDEX IF EXISTS public.nvswitch_vendor_idx;
DROP TABLE IF EXISTS public.nvswitch;
