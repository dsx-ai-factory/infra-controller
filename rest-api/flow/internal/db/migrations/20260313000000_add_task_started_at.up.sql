-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Add started_at to record when a task actually begins execution.
ALTER TABLE task ADD COLUMN IF NOT EXISTS started_at TIMESTAMPTZ;
