-- Store the expected racks and devices that form an NVLink domain.
CREATE TABLE expected_rack_groups (
    rack_group_id varchar(128) PRIMARY KEY,
    topology varchar(128) NOT NULL,
    rack_ids jsonb NOT NULL,
    members jsonb NOT NULL,
    metadata_name varchar(256) NOT NULL DEFAULT '',
    metadata_description varchar(1024) NOT NULL DEFAULT '',
    metadata_labels jsonb NOT NULL DEFAULT '{}'
);
