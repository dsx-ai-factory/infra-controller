-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Persist the latest Core aggregate health snapshot with Flow's rack and
-- component inventory read models.
ALTER TABLE component ADD COLUMN health JSONB;
ALTER TABLE rack ADD COLUMN health JSONB;
