-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE public.task DROP CONSTRAINT IF EXISTS task_rule_fkey;
ALTER TABLE public.task DROP COLUMN IF EXISTS applied_rule_id;
