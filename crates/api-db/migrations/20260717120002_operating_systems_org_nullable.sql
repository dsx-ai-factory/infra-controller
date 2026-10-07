-- Allow an operating_system definition to exist without an owning tenant
-- organization (e.g. provider-owned OSes). Absence of an org is now represented
-- as NULL.
ALTER TABLE operating_systems
    ALTER COLUMN org DROP NOT NULL;
