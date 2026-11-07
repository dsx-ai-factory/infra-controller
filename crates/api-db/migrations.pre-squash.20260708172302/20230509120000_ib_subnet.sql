CREATE TABLE ibsubnet_controller_lock
(
    id uuid DEFAULT gen_random_uuid() NOT NULL
);

CREATE TABLE ib_subnets
(
    id             uuid                  DEFAULT gen_random_uuid() NOT NULL,

    -- Spec
    name           VARCHAR      NOT NULL UNIQUE,
    vpc_id         uuid         NOT NULL,
    config_version VARCHAR(64)  NOT NULL,

    -- Status
    status  jsonb NULL,

    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted TIMESTAMPTZ,

    -- The state of IB Subnet:
    --   * Initializing: 0
    --   * Initialized: 1
    --   * Ready: 2
    --   * Error: 3
    --   * Deleting: 4
    --   * Deleted: 5
    controller_state_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268'),
    controller_state         jsonb       NOT NULL DEFAULT ('{"state":"initializing"}'),

    PRIMARY KEY (id),
    FOREIGN KEY (vpc_id) REFERENCES vpcs (id)
);
