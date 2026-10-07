-- Introduces iteration ID and queued objects tables for all state controllers

CREATE TABLE machine_state_controller_iteration_ids(
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE TABLE network_segments_controller_iteration_ids(
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE TABLE ib_partition_controller_iteration_ids(
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE TABLE dpa_interfaces_controller_iteration_ids(
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE TABLE power_shelf_controller_iteration_ids(
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE TABLE switch_controller_iteration_ids(
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE TABLE rack_controller_iteration_ids(
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Queued object tables for all resources

CREATE TABLE machine_state_controller_queued_objects(
    object_id VARCHAR PRIMARY KEY,
    iteration_id BIGINT
);

CREATE TABLE network_segments_controller_queued_objects(
    object_id VARCHAR PRIMARY KEY,
    iteration_id BIGINT
);

CREATE TABLE ib_partition_controller_queued_objects(
    object_id VARCHAR PRIMARY KEY,
    iteration_id BIGINT
);

CREATE TABLE dpa_interfaces_controller_queued_objects(
    object_id VARCHAR PRIMARY KEY,
    iteration_id BIGINT
);

CREATE TABLE power_shelf_controller_queued_objects(
    object_id VARCHAR PRIMARY KEY,
    iteration_id BIGINT
);

CREATE TABLE switch_controller_queued_objects(
    object_id VARCHAR PRIMARY KEY,
    iteration_id BIGINT
);

CREATE TABLE rack_controller_queued_objects(
    object_id VARCHAR PRIMARY KEY,
    iteration_id BIGINT
);