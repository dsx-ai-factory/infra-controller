-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

DO $$
BEGIN
    -- The original operation_run_target migration may already be applied with
    -- component_filter. Keep that migration stable and advance the schema here.
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = current_schema()
            AND table_name = 'operation_run_target'
            AND column_name = 'component_filter'
    ) AND NOT EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = current_schema()
            AND table_name = 'operation_run_target'
            AND column_name = 'components_by_type'
    ) THEN
        ALTER TABLE operation_run_target
            RENAME COLUMN component_filter TO components_by_type;
    END IF;
END $$;
