-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Keep the originating log trace available to tasks dispatched after a restart.
ALTER TABLE operation_run ADD COLUMN metadata jsonb;
