-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

--
-- Name: component_drift; Type: TABLE; Schema: public; Owner: postgres
-- Stores per-component validation drift detected by the inventory loop.
-- Each inventory loop cycle rebuilds this table with current mismatches.
--
-- drift_type: 'missing_in_expected', 'missing_in_actual', 'mismatch'
-- component_id: NULL for missing_in_expected (exists in source system but not in local DB)
-- external_id:  NULL for missing_in_actual without external_id
--

CREATE TABLE public.component_drift (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    component_id uuid,
    external_id character varying,
    drift_type character varying(32) NOT NULL,
    diffs jsonb NOT NULL DEFAULT '[]'::jsonb,
    checked_at timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP
);

--
-- Name: component_drift component_drift_pkey; Type: CONSTRAINT; Schema: public; Owner: postgres
--

ALTER TABLE ONLY public.component_drift
    ADD CONSTRAINT component_drift_pkey PRIMARY KEY (id);
