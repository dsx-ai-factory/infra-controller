-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Inventory provides domain identity without a display name.
ALTER TABLE nvldomain ALTER COLUMN name DROP NOT NULL;
