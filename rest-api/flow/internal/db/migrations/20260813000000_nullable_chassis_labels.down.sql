-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- The empty string is how a missing label was represented while these columns
-- were NOT NULL, so it is the inverse of the model's nullzero mapping. It is
-- not distinct the way NULL is, so rows that collapse onto the same pair
-- cannot coexist under the restored constraint. Refuse the downgrade rather
-- than invent labels to tell them apart.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM component
        GROUP BY coalesce(manufacturer, ''), coalesce(serial_number, '')
        HAVING count(*) > 1
    ) THEN
        RAISE EXCEPTION 'component rows share a (manufacturer, serial_number) pair once NULL collapses to the empty string'
            USING HINT = 'Give the affected rows distinct labels, or remove them, before downgrading.';
    END IF;

    IF EXISTS (
        SELECT 1 FROM rack
        GROUP BY coalesce(manufacturer, ''), coalesce(serial_number, '')
        HAVING count(*) > 1
    ) THEN
        RAISE EXCEPTION 'rack rows share a (manufacturer, serial_number) pair once NULL collapses to the empty string'
            USING HINT = 'Give the affected rows distinct labels, or remove them, before downgrading.';
    END IF;
END $$;

UPDATE component SET manufacturer = '' WHERE manufacturer IS NULL;
UPDATE component SET serial_number = '' WHERE serial_number IS NULL;

ALTER TABLE component
    ALTER COLUMN manufacturer SET NOT NULL,
    ALTER COLUMN serial_number SET NOT NULL;

UPDATE rack SET manufacturer = '' WHERE manufacturer IS NULL;
UPDATE rack SET serial_number = '' WHERE serial_number IS NULL;

ALTER TABLE rack
    ALTER COLUMN manufacturer SET NOT NULL,
    ALTER COLUMN serial_number SET NOT NULL;
