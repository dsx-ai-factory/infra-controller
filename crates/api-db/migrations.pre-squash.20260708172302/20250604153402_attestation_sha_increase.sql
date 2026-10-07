-- Add migration script here
alter table measurement_bundles_values alter column sha256 type varchar(128); -- this is to fit SHA512 if needed
alter table measurement_reports_values alter column sha256 type varchar(128);

ALTER TABLE measurement_bundles_values RENAME COLUMN sha256 TO sha_any;
ALTER TABLE measurement_reports_values RENAME COLUMN sha256 TO sha_any;


