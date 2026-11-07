-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Adds Flow's per-component leak status. The leak-detection loop sets it
-- from core's tray-leak-detection health alert each cycle; rows the loop
-- has not yet evaluated keep the 'UNKNOWN' default.
ALTER TABLE component
    ADD COLUMN leak_status character varying(16) NOT NULL DEFAULT 'UNKNOWN';
