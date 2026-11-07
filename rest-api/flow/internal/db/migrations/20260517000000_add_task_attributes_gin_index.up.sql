-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- GIN index on task.attributes to support component-targeted task queries.
--
-- The intended query pattern (used by ListTasks when ComponentID is set) is:
--
--   attributes @? '$.components_by_type.*[*] ? (@ == "<uuid>")'
--
-- which finds tasks that target a given component UUID regardless of which
-- component-type bucket it lives under in attributes.components_by_type.
--
-- jsonb_path_ops is preferred over the default jsonb_ops here: it produces a
-- smaller, faster index and supports the operators we actually use (@>, @?,
-- @@). It does not support the key-existence ? operator, which we don't need.
CREATE INDEX IF NOT EXISTS idx_task_attributes_gin
    ON public.task USING GIN (attributes jsonb_path_ops);
