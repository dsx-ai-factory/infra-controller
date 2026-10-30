-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Preserve the source Rack independently of later component moves.
-- Older events retain NULL because their original Rack cannot be reconstructed.
ALTER TABLE events ADD COLUMN rack_id uuid;
CREATE INDEX events_rack_id_created_at_idx ON events (rack_id, created_at DESC, id DESC);
