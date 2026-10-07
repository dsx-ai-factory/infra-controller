-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM rack
        GROUP BY name
        HAVING count(*) > 1
    ) THEN
        RAISE EXCEPTION 'rack rows have duplicate names and cannot restore rack_name_idx uniqueness';
    END IF;
END
$$;

DROP INDEX rack_name_idx;

ALTER TABLE rack
    ADD CONSTRAINT rack_name_idx UNIQUE (name);
