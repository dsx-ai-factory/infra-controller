-- Tenant-owned DHCP values that replace NICo's defaults in DHCP replies.
CREATE TABLE dhcp_option_sets (
    id uuid PRIMARY KEY,
    tenant_organization_id varchar(64) NOT NULL REFERENCES tenants(organization_id),
    name varchar(256) NOT NULL,
    description varchar(1024) NOT NULL DEFAULT '',
    labels jsonb NOT NULL DEFAULT '{}',
    options jsonb NOT NULL,
    version varchar(64) NOT NULL,
    created timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);
