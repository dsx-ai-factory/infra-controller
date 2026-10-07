
CREATE TABLE network_security_groups (
    id                      character varying(64) NOT NULL DEFAULT 'INVALID_NETWORK_SECURITY_GROUP',
    tenant_organization_id  character varying(64),
    name                    character varying(256) NOT NULL,
    description             character varying(1024) NOT NULL DEFAULT '',
    labels                  jsonb NOT NULL DEFAULT '{}'::jsonb,
    rules                   jsonb NOT NULL DEFAULT '[]'::jsonb,
    version                 character varying(64) NOT NULL DEFAULT 'V1-T0'::character varying,
    created                 timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    deleted                 timestamp with time zone,
    created_by              character varying(64),
    updated_by              character varying(64)
);
ALTER TABLE ONLY network_security_groups ADD CONSTRAINT network_security_groups_pkey PRIMARY KEY (id);
ALTER TABLE ONLY network_security_groups ADD CONSTRAINT network_security_groups_tenant_id_fkey FOREIGN KEY (tenant_organization_id) REFERENCES tenants(organization_id);


/**** VPC ****/
ALTER TABLE ONLY vpcs ADD COLUMN network_security_group_id character varying(64);
ALTER TABLE ONLY vpcs ADD CONSTRAINT vpcs_network_security_group_id_fkey FOREIGN KEY (network_security_group_id) REFERENCES network_security_groups(id);


/**** Instance ****/
ALTER TABLE ONLY instances ADD COLUMN network_security_group_id character varying(64);
ALTER TABLE ONLY instances ADD CONSTRAINT instances_network_security_group_id_fkey FOREIGN KEY (network_security_group_id) REFERENCES network_security_groups(id);