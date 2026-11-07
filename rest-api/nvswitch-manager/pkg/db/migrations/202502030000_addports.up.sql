-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

--
-- Add port columns to nvswitch table for custom BMC/NVOS ports
-- This allows connecting through tunnels or non-standard ports
--

-- Add BMC port (default 443 for Redfish HTTPS)
ALTER TABLE public.nvswitch
    ADD COLUMN bmc_port integer NOT NULL DEFAULT 443;

-- Add NVOS port (default 22 for SSH)
ALTER TABLE public.nvswitch
    ADD COLUMN nvos_port integer NOT NULL DEFAULT 22;

-- Update existing rows to have default ports (redundant but explicit)
UPDATE public.nvswitch SET bmc_port = 443 WHERE bmc_port IS NULL;
UPDATE public.nvswitch SET nvos_port = 22 WHERE nvos_port IS NULL;
