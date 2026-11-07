-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Create operation_rules table
-- Rules are templates that define how operations should be performed

CREATE TABLE public.operation_rules (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    name character varying(128) NOT NULL,
    description text,
    operation_type character varying(64) NOT NULL,
    operation_code character varying(64) NOT NULL,
    rule_definition jsonb NOT NULL,
    is_default boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    updated_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,

    CONSTRAINT operation_rules_pkey PRIMARY KEY (id)
);

-- Index for searching by name (no unique constraint - names can be duplicated)
CREATE INDEX idx_operation_rules_name
    ON public.operation_rules(name);

-- Index for querying rules by operation type and operation code
CREATE INDEX idx_operation_rules_operation_type_code
    ON public.operation_rules(operation_type, operation_code);

-- Partial unique constraint: only one default rule per (operation_type, operation_code)
-- This ensures there's at most one default fallback rule for each specific operation
CREATE UNIQUE INDEX idx_operation_rules_unique_default
    ON public.operation_rules(operation_type, operation_code)
    WHERE is_default = true;

-- Create rack_rule_associations table
-- Maps racks to specific rules for each operation type

CREATE TABLE public.rack_rule_associations (
    rack_id uuid NOT NULL,
    operation_type character varying(64) NOT NULL,
    operation_code character varying(64) NOT NULL,
    rule_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    updated_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,

    CONSTRAINT rack_rule_associations_pkey PRIMARY KEY (rack_id, operation_type, operation_code),
    CONSTRAINT rack_rule_associations_rack_fkey FOREIGN KEY (rack_id)
        REFERENCES rack(id) ON DELETE CASCADE,
    CONSTRAINT rack_rule_associations_rule_fkey FOREIGN KEY (rule_id)
        REFERENCES operation_rules(id) ON DELETE CASCADE
);

-- Index for looking up associations by rule_id (to find which racks use a rule)
CREATE INDEX idx_rack_rule_associations_rule_id
    ON public.rack_rule_associations(rule_id);

-- Index for looking up by rack and operation type and code (primary query pattern)
CREATE INDEX idx_rack_rule_associations_rack_operation
    ON public.rack_rule_associations(rack_id, operation_type, operation_code);
