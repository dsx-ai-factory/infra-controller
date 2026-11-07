-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

DROP TRIGGER IF EXISTS component_set_updated_at ON component;

ALTER TABLE component
    DROP COLUMN IF EXISTS updated_at;
