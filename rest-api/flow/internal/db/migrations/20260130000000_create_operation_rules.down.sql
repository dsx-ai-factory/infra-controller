-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Drop rack_rule_associations first (has FK to operation_rules)
DROP TABLE IF EXISTS public.rack_rule_associations;

-- Drop operation_rules table
DROP TABLE IF EXISTS public.operation_rules;
