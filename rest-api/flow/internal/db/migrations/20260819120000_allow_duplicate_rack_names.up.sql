-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Core treats an expected rack's name as optional metadata and does not
-- require it to be unique. Flow identifies mirrored racks by external_id, so
-- retain an index for name filters without rejecting duplicate Core names.
ALTER TABLE rack
    DROP CONSTRAINT rack_name_idx;

CREATE INDEX rack_name_idx ON rack (name);

-- Older expected-rack reconciliation wrote the shared Location field under
-- "datacenter". Move existing values to the canonical JSON key immediately so
-- reads remain correct before the next successful authoritative sync.
UPDATE rack
SET location = CASE
    WHEN location ? 'data_center' THEN location - 'datacenter'
    ELSE (location - 'datacenter') || jsonb_build_object('data_center', location->'datacenter')
END
WHERE location ? 'datacenter';
