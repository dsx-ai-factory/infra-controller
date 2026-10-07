CREATE TABLE site_explorer_lock(
    id uuid DEFAULT gen_random_uuid() NOT NULL
);

CREATE TABLE explored_endpoints(
    -- The node we explored
    address inet NOT NULL PRIMARY KEY,
    -- Holds data discovered about the endpoint
    exploration_report jsonb NOT NULL,
    -- Gets updated every time the report is updated
    version VARCHAR(64) NOT NULL
);
