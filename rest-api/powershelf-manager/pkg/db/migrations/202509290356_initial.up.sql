-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

--
-- Name: pmc; Type: TABLE; Schema: public
-- Matches Go model: pkg/db/model/pmc.go
--

CREATE TABLE public.pmc (
    mac_address macaddr NOT NULL,
    vendor integer NOT NULL,
    ip_address inet NOT NULL
);

ALTER TABLE ONLY public.pmc
    ADD CONSTRAINT pmc_pkey PRIMARY KEY (mac_address);

ALTER TABLE ONLY public.pmc
    ADD CONSTRAINT pmc_mac_address_unique UNIQUE (mac_address);

ALTER TABLE ONLY public.pmc
    ADD CONSTRAINT pmc_ip_address_unique UNIQUE (ip_address);

-- Index on vendor for filtering by vendor type
CREATE INDEX pmc_vendor_idx ON public.pmc (vendor);

-- SECTION

--
-- Name: firmware_update; Type: TABLE; Schema: public
-- Matches Go model: pkg/db/model/firmware_update.go
--

CREATE TABLE public.firmware_update (
    pmc_mac_address macaddr NOT NULL,
    component character varying NOT NULL,
    version_from character varying NOT NULL,
    version_to character varying NOT NULL,
    state character varying NOT NULL,
    last_transition_time timestamp with time zone NOT NULL,
    job_id character varying,
    error_message character varying,
    created_at timestamp with time zone NOT NULL DEFAULT NOW(),
    updated_at timestamp with time zone NOT NULL DEFAULT NOW()
);

ALTER TABLE ONLY public.firmware_update
    ADD CONSTRAINT firmware_update_pkey PRIMARY KEY (pmc_mac_address, component);

-- Index on state for GetAllPendingFirmwareUpdates() which filters by state
CREATE INDEX firmware_update_state_idx ON public.firmware_update (state);

-- Index on created_at for ORDER BY created_at DESC queries
CREATE INDEX firmware_update_created_at_idx ON public.firmware_update (created_at DESC);

-- Composite index for the common query pattern: filter by state + order by created_at
CREATE INDEX firmware_update_state_created_idx ON public.firmware_update (state, created_at DESC);
