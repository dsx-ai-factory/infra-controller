-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE task DROP COLUMN IF EXISTS attributes;
ALTER TABLE task ADD COLUMN IF NOT EXISTS component_uuids jsonb;
