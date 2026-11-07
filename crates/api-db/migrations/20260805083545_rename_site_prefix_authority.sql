-- PostgreSQL keeps the enum member's identity when its label changes, so
-- existing rows and expressions retain their meaning without a table rewrite.
ALTER TYPE site_prefix_authority
    RENAME VALUE 'configured' TO 'operator_managed';

ALTER TABLE site_prefixes
    RENAME CONSTRAINT site_prefixes_configured_lifecycle_check
    TO site_prefixes_operator_managed_lifecycle_check;

ALTER INDEX site_prefixes_configured_prefix_key
    RENAME TO site_prefixes_operator_managed_prefix_key;
