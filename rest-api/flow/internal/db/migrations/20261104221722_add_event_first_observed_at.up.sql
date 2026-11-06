-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Older event records have no source observation timestamp.
ALTER TABLE events ADD COLUMN first_observed_at timestamp with time zone;
