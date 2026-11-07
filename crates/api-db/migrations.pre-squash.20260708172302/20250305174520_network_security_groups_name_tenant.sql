
/* We'll protect against duplicates in code as well.  This is just extra protection. */
DROP INDEX IF EXISTS nsg_unique_name;

/* Only need to be unique per tenant*/
CREATE UNIQUE INDEX nsg_unique_name ON network_security_groups (name, tenant_organization_id) WHERE (deleted) IS NULL;
