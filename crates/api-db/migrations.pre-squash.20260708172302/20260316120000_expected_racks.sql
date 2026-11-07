-- Expected racks table. An expected rack declares a rack by its rack_id and
-- rack_type. The rack_type determines how many compute trays, switches, and
-- power shelves the rack is expected to contain.
CREATE TABLE IF NOT EXISTS expected_racks (
    rack_id VARCHAR(128) PRIMARY KEY,
    rack_type VARCHAR(256) NOT NULL,
    metadata_name VARCHAR(256) DEFAULT '',
    metadata_description VARCHAR(1024) DEFAULT '',
    metadata_labels JSONB DEFAULT '{}'
);
