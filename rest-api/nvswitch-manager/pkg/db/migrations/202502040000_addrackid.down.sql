-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Remove rack_id column from nvswitch table

DROP INDEX IF EXISTS nvswitch_rack_id_idx;
ALTER TABLE nvswitch DROP COLUMN IF EXISTS rack_id;
