-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = current_schema()
            AND table_name = 'operation_run_target'
            AND column_name = 'components_by_type'
    ) AND NOT EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = current_schema()
            AND table_name = 'operation_run_target'
            AND column_name = 'component_filter'
    ) THEN
        ALTER TABLE operation_run_target
            RENAME COLUMN components_by_type TO component_filter;
    END IF;
END $$;
