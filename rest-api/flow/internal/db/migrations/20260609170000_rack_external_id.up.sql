-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Adds the Core-side stable rack identifier (ExpectedRack.rack_id, e.g.
-- "a12") so the new expected-inventory mirror can match Flow racks against
-- Core unambiguously and idempotently. The column is nullable: racks created
-- before the mirror runs (or via the legacy ingestion gRPC) start without it
-- and are adopted on the first sync that finds a Core match by
-- (manufacturer, serial_number). The partial unique index leaves NULL rows
-- unconstrained but rejects duplicate external_id assignments.
ALTER TABLE rack
    ADD COLUMN external_id TEXT;

CREATE UNIQUE INDEX rack_external_id_idx
    ON rack (external_id)
    WHERE external_id IS NOT NULL;
