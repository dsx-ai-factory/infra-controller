-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Derived from pg_dump on previous bun go based creation

--
-- Name: bmc; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.bmc (
    mac_address character varying NOT NULL,
    type character varying NOT NULL,
    component_id uuid NOT NULL,
    ip_address character varying,
    "user" character varying,
    password character varying
);

--
-- Name: component; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.component (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    name character varying,
    type character varying(16) DEFAULT 'Compute'::character varying,
    manufacturer character varying NOT NULL,
    model character varying,
    serial_number character varying NOT NULL,
    description jsonb,
    firmware_version character varying,
    rack_id uuid NOT NULL,
    slot_id bigint,
    tray_index bigint,
    host_id bigint,
    associated_id character varying,
    ingested_at timestamp with time zone,
    deleted_at timestamp with time zone
);

--
-- Name: nvldomain; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.nvldomain (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    name character varying NOT NULL,
    created_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    deleted_at timestamp with time zone
);

--
-- Name: rack; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.rack (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    name character varying NOT NULL,
    manufacturer character varying NOT NULL,
    serial_number character varying NOT NULL,
    description jsonb,
    location jsonb,
    nvldomain_id uuid,
    status character varying(16) DEFAULT 'new'::character varying,
    created_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    updated_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    ingested_at timestamp with time zone,
    deleted_at timestamp with time zone
);

--
-- Name: bmc bmc_pkey; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.bmc
    ADD CONSTRAINT bmc_pkey PRIMARY KEY (mac_address);


--
-- Name: component component_manufacturer_serial_idx; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.component
    ADD CONSTRAINT component_manufacturer_serial_idx UNIQUE (manufacturer, serial_number);

--
-- Name: component component_pkey; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.component
    ADD CONSTRAINT component_pkey PRIMARY KEY (id);

--
-- Name: nvldomain nvldomain_name_idx; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.nvldomain
    ADD CONSTRAINT nvldomain_name_idx UNIQUE (name);


--
-- Name: nvldomain nvldomain_pkey; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.nvldomain
    ADD CONSTRAINT nvldomain_pkey PRIMARY KEY (id);

--
-- Name: rack rack_manufacturer_serial_idx; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.rack
    ADD CONSTRAINT rack_manufacturer_serial_idx UNIQUE (manufacturer, serial_number);

--
-- Name: rack rack_name_idx; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.rack
    ADD CONSTRAINT rack_name_idx UNIQUE (name);

--
-- Name: rack rack_pkey; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.rack
    ADD CONSTRAINT rack_pkey PRIMARY KEY (id);
