-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Scope derived drift rows so each actual-inventory source can refresh independently.
-- NULL remains accepted while predecessor Flow instances may still write the table.
ALTER TABLE component_drift ADD COLUMN component_type VARCHAR(32);

UPDATE component_drift AS drift
SET component_type = component.type
FROM component
WHERE drift.component_id = component.id;
