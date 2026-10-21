-- Keep the site's identity and exact registration requests across restarts.
CREATE TABLE resource_pool_site (
    singleton BOOLEAN PRIMARY KEY CHECK (singleton),
    site_id UUID NOT NULL
);

CREATE TABLE resource_pool_registration (
    binding_id UUID NOT NULL REFERENCES resource_pool_binding(binding_id),
    revision BIGINT NOT NULL CHECK (revision > 0),
    request BYTEA NOT NULL,
    accepted BOOLEAN NOT NULL,
    PRIMARY KEY (binding_id, revision)
);

CREATE UNIQUE INDEX resource_pool_registration_pending
    ON resource_pool_registration (binding_id) WHERE NOT accepted;
