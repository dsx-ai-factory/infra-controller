CREATE TABLE dns_record_metadata (
    id UUID PRIMARY KEY,
    record_type_id INTEGER NOT NULL,
    ttl INTEGER DEFAULT 3600,
    FOREIGN KEY (record_type_id) REFERENCES dns_record_types(id) ON DELETE RESTRICT
);
