-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

--
-- Name: task; Type: TABLE; Schema: public; Owner: postgres
--

CREATE TABLE public.task (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    type character varying(64) NOT NULL,
    executor_type character varying(64) NOT NULL,
    information jsonb,
    description text,
    rack_id uuid NOT NULL,
    component_ids jsonb,
    execution_id character varying NOT NULL,
    status character varying(32) NOT NULL,
    message text,
    created_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    updated_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    finished_at timestamp with time zone
);

--
-- Name: task task_pkey; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.task
    ADD CONSTRAINT task_pkey PRIMARY KEY (id);
