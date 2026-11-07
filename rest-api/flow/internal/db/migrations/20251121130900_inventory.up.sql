-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE component 
    ADD COLUMN machine_id TEXT,
    ADD COLUMN power_state INT;

CREATE INDEX component_machine_id ON component (machine_id);
