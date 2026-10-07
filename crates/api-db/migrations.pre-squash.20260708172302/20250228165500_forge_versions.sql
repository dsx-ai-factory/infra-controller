CREATE TABLE forge_versions (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
    version     VARCHAR NOT NULL,
    superseded  timestamp with time zone NULL,
    first_seen  timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE UNIQUE INDEX idx_forge_versions_version ON forge_versions (version);
