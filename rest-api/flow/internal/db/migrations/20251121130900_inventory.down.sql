-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

DROP INDEX IF EXISTS component_machine_id;

ALTER TABLE component
    DROP COLUMN IF EXISTS machine_id,
    DROP COLUMN IF EXISTS power_state;
