-- Add migration script here
ALTER TABLE domains drop CONSTRAINT IF EXISTS valid_domain_name_regex;
ALTER TABLE domains drop CONSTRAINT IF EXISTS domain_name_lower_case;
