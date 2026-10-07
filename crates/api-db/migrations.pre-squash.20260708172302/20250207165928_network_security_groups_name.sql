
/* We'll protect against duplicates in code as well.  This is just extra protection. */
CREATE UNIQUE INDEX nsg_unique_name ON network_security_groups (name) WHERE (deleted) IS NULL;
