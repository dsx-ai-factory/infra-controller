-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

--
-- Remove port columns from nvswitch table
--

ALTER TABLE public.nvswitch DROP COLUMN IF EXISTS bmc_port;
ALTER TABLE public.nvswitch DROP COLUMN IF EXISTS nvos_port;
