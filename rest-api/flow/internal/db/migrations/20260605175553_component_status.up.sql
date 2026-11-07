-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Adds Flow's per-component status: phase / reason / blocked_operations,
-- computed by the inventory loop from core's controller_state. Stored as
-- a single jsonb column so the shape can evolve without further DDL.
ALTER TABLE component
    ADD COLUMN status jsonb;
