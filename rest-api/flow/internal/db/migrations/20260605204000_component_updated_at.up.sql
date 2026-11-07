-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Adds a generic updated_at column to the component table, mirroring the
-- convention already used by rack, nvldomain, task_schedule, etc. The
-- shared set_updated_at trigger stamps the column on every UPDATE so
-- callers have one freshness signal regardless of which field changed
-- (power_state, firmware_version, status, description, ...).
ALTER TABLE component
    ADD COLUMN updated_at timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP;

CREATE TRIGGER component_set_updated_at
    BEFORE UPDATE ON component
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
