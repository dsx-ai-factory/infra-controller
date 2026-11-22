-- Keep the tenant and cancellation decision while a VPC create is unfinished.
-- A cancelled ID stays reserved so a delayed create cannot revive it.
CREATE TABLE vpc_creation_requests (
    vpc_id UUID PRIMARY KEY,
    tenant_organization_id TEXT NOT NULL,
    cancelled BOOLEAN NOT NULL DEFAULT false
);
