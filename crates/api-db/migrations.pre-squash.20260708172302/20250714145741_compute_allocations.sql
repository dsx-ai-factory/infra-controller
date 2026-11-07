-- Create a table to be used for allocation locks.
-- There won't ever be anything in here, but it's an easy way to get a
-- common/consistent integer for try_lock with a simple select like
-- SELECT 'compute_allocation_lock'::regclass::oid;
CREATE TABLE compute_allocation_lock(
    id uuid DEFAULT gen_random_uuid() NOT NULL
);

CREATE TABLE compute_allocations (
    id                       uuid NOT NULL,
    tenant_organization_id   character varying(64) NOT NULL,
    name                     character varying NOT NULL,
    version                  character varying(64) NOT NULL DEFAULT 'V0-T0'::character varying,
    labels                   jsonb NOT NULL DEFAULT '{}'::jsonb,
    description              character varying(256) NOT NULL DEFAULT '',    

    instance_type_id         character varying(64) NOT NULL,
    count                    int NOT NULL DEFAULT 0,
    
    created                  timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted                  timestamp with time zone,
    created_by               character varying(64),
    updated_by               character varying(64)
);
ALTER TABLE ONLY compute_allocations ADD CONSTRAINT compute_allocations_pkey PRIMARY KEY (id);
CREATE UNIQUE INDEX compute_allocations_unique_name ON compute_allocations (name, tenant_organization_id) WHERE (deleted) IS NULL;
ALTER TABLE ONLY compute_allocations ADD CONSTRAINT compute_allocations_instance_type_id_fkey FOREIGN KEY (instance_type_id) REFERENCES instance_types(id);
ALTER TABLE ONLY compute_allocations ADD CONSTRAINT compute_allocations_tenant_id_fkey FOREIGN KEY (tenant_organization_id) REFERENCES tenants(organization_id);
