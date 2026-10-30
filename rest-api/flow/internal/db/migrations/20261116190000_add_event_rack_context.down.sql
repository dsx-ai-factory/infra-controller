-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

DROP INDEX events_rack_id_created_at_idx;
ALTER TABLE events DROP COLUMN rack_id;
