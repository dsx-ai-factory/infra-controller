-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Refuse rollback until unnamed domains have been explicitly named.
ALTER TABLE nvldomain ALTER COLUMN name SET NOT NULL;
