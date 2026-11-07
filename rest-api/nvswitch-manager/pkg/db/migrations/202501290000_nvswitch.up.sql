-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

--
-- Name: nvswitch; Type: TABLE; Schema: public
-- Matches Go model: pkg/db/model/bmc.go (NVSwitch struct)
-- NV-Switch tray with UUID primary key and separate BMC/NVOS subsystem identities
--

CREATE TABLE public.nvswitch (
    uuid uuid NOT NULL DEFAULT gen_random_uuid(),
    vendor integer NOT NULL,
    -- BMC subsystem (Redfish access)
    bmc_mac_address macaddr NOT NULL,
    bmc_ip_address inet NOT NULL,
    -- NVOS subsystem (SSH access)
    nvos_mac_address macaddr NOT NULL,
    nvos_ip_address inet NOT NULL
);

-- Primary key on UUID
ALTER TABLE ONLY public.nvswitch
    ADD CONSTRAINT nvswitch_pkey PRIMARY KEY (uuid);

-- Unique constraints on all identity fields
ALTER TABLE ONLY public.nvswitch
    ADD CONSTRAINT nvswitch_bmc_mac_address_unique UNIQUE (bmc_mac_address);

ALTER TABLE ONLY public.nvswitch
    ADD CONSTRAINT nvswitch_bmc_ip_address_unique UNIQUE (bmc_ip_address);

ALTER TABLE ONLY public.nvswitch
    ADD CONSTRAINT nvswitch_nvos_mac_address_unique UNIQUE (nvos_mac_address);

ALTER TABLE ONLY public.nvswitch
    ADD CONSTRAINT nvswitch_nvos_ip_address_unique UNIQUE (nvos_ip_address);

-- Index on vendor for filtering by vendor type
CREATE INDEX nvswitch_vendor_idx ON public.nvswitch (vendor);

-- SECTION

--
-- Name: firmware_update; Type: TABLE; Schema: public
-- Matches Go model: pkg/firmwaremanager/store_postgres.go (FirmwareUpdateModel)
-- Tracks firmware update operations for NV-Switch components (FIRMWARE, CPLD, NVOS)
-- Uses UUID primary key to allow multiple updates per switch/component over time
--

CREATE TABLE public.firmware_update (
    id uuid NOT NULL DEFAULT gen_random_uuid(),
    switch_uuid uuid NOT NULL,
    component character varying NOT NULL,
    bundle_version character varying NOT NULL,
    strategy character varying NOT NULL,
    state character varying NOT NULL DEFAULT 'QUEUED',
    version_from character varying,
    version_to character varying NOT NULL,
    version_actual character varying,
    task_uri character varying,
    error_message character varying,
    -- Sequencing fields for multi-component updates
    bundle_update_id uuid,              -- Groups related updates together
    sequence_order integer DEFAULT 0,   -- Order within bundle update (1, 2, 3...)
    predecessor_id uuid,                -- ID of update that must complete first
    created_at timestamp with time zone NOT NULL DEFAULT NOW(),
    updated_at timestamp with time zone NOT NULL DEFAULT NOW()
);

-- Primary key on UUID
ALTER TABLE ONLY public.firmware_update
    ADD CONSTRAINT firmware_update_pkey PRIMARY KEY (id);

-- Foreign key to nvswitch table
ALTER TABLE ONLY public.firmware_update
    ADD CONSTRAINT firmware_update_switch_uuid_fkey 
    FOREIGN KEY (switch_uuid) REFERENCES public.nvswitch(uuid) ON DELETE CASCADE;

-- Index on state for ClaimQueued() which filters by state='QUEUED'
CREATE INDEX firmware_update_state_idx ON public.firmware_update (state);

-- Index on created_at for ORDER BY created_at ASC queries (workers claim oldest first)
CREATE INDEX firmware_update_created_at_idx ON public.firmware_update (created_at ASC);

-- Composite index for the ClaimQueued query: filter by state + order by created_at
CREATE INDEX firmware_update_state_created_idx ON public.firmware_update (state, created_at ASC);

-- Index on switch_uuid for GetBySwitch() lookups
CREATE INDEX firmware_update_switch_uuid_idx ON public.firmware_update (switch_uuid);

-- Composite index for GetActive(): find non-terminal updates for a switch/component
CREATE INDEX firmware_update_active_idx ON public.firmware_update (switch_uuid, component) 
    WHERE state NOT IN ('COMPLETED', 'FAILED');

-- Index on bundle_update_id for grouping related updates
CREATE INDEX firmware_update_bundle_idx ON public.firmware_update (bundle_update_id)
    WHERE bundle_update_id IS NOT NULL;

-- Index on predecessor_id for dependency lookups
CREATE INDEX firmware_update_predecessor_idx ON public.firmware_update (predecessor_id)
    WHERE predecessor_id IS NOT NULL;
