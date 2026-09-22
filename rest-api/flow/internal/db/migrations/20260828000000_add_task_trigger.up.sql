-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Store generic trigger provenance directly on tasks.
ALTER TABLE task
    ADD COLUMN trigger_type VARCHAR(64),
    ADD COLUMN trigger_id UUID,
    ADD CONSTRAINT task_trigger_complete
        CHECK (
            (trigger_type IS NULL AND trigger_id IS NULL)
            OR (
                trigger_type IS NOT NULL
                AND (
                    (trigger_type = 'api' AND trigger_id IS NULL)
                    OR (
                        trigger_type = 'event_rule_execution'
                        AND trigger_id IS NOT NULL
                        AND trigger_id <> '00000000-0000-0000-0000-000000000000'::uuid
                    )
                )
            )
        ) NOT VALID;

-- Validate task_trigger_complete in a later deployment transaction, after this
-- migration's table lock has been released.

CREATE INDEX task_trigger_idx ON task (trigger_type, trigger_id)
WHERE trigger_type IS NOT NULL;
