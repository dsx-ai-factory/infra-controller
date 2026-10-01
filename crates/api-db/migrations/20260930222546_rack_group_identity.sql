-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Preserve the group selected when an expected rack is created, then carry it into discovery.
ALTER TABLE expected_racks ADD COLUMN rack_group_id varchar;
ALTER TABLE racks ADD COLUMN rack_group_id varchar;
