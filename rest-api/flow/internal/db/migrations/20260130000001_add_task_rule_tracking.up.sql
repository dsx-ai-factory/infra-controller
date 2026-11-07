-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE public.task ADD COLUMN applied_rule_id uuid;

ALTER TABLE public.task ADD CONSTRAINT task_rule_fkey
    FOREIGN KEY (applied_rule_id) REFERENCES operation_rules(id) ON DELETE SET NULL;

CREATE INDEX idx_task_applied_rule ON public.task(applied_rule_id);
