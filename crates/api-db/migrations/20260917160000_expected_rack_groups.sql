-- Store the expected racks and devices that form an NVLink domain.
CREATE TABLE expected_rack_groups (
    rack_group_id varchar(128) PRIMARY KEY,
    topology varchar(128) NOT NULL,
    rack_ids jsonb NOT NULL,
    members jsonb NOT NULL,
    metadata_name varchar(255) NOT NULL,
    metadata_description text NOT NULL,
    metadata_labels jsonb NOT NULL
);
