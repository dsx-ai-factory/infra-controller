-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Chassis manufacturer and serial number become descriptive metadata: a
-- mirrored rack is identified by external_id and a mirrored component by its
-- host BMC MAC address. Both (manufacturer, serial_number) unique constraints
-- stay in place, and NULLs are distinct in Postgres, so a row missing either
-- half occupies no slot in them.
ALTER TABLE component
    ALTER COLUMN manufacturer DROP NOT NULL,
    ALTER COLUMN serial_number DROP NOT NULL;

ALTER TABLE rack
    ALTER COLUMN manufacturer DROP NOT NULL,
    ALTER COLUMN serial_number DROP NOT NULL;
