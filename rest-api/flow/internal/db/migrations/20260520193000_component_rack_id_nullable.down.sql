-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Reverts component.rack_id back to NOT NULL. Unattached components must be
-- removed before downgrading; they cannot be assigned a rack retroactively.
DELETE FROM component WHERE rack_id IS NULL;
ALTER TABLE component ALTER COLUMN rack_id SET NOT NULL;
