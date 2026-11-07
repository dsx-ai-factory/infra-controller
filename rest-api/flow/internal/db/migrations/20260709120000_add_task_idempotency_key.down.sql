-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

DROP INDEX IF EXISTS task_idempotency_key_unique;
ALTER TABLE task DROP COLUMN IF EXISTS idempotency_key;
