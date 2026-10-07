-- Add migration script here

CREATE TYPE spdm_attestation_status_t AS ENUM ('not_started', 'started', 'not_supported', 'device_list_mismatch', 'completed');

-- Create the spdm_machine_attestation table
CREATE TABLE spdm_machine_attestation (
    machine_id VARCHAR UNIQUE NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL,
    started_at TIMESTAMPTZ,
    canceled_at TIMESTAMPTZ,
    state JSONB NOT NULL,
    -- If state version time is less than requested_at, indicates that a new request is received.
    -- State machine should reset the state and start over
    state_version VARCHAR NOT NULL,
    state_outcome JSONB,
    attestation_status spdm_attestation_status_t NOT NULL DEFAULT 'not_started',

    CONSTRAINT fk_machine_id
        FOREIGN KEY (machine_id)
        REFERENCES machines(id)
        ON DELETE CASCADE  -- force-delete handling.
        ON UPDATE CASCADE  -- Update predicted host id to permanent host id scenario.
);

CREATE TABLE attestation_state_controller_lock(
    id uuid DEFAULT gen_random_uuid() NOT NULL
);

-- Create the spdm_machine_devices_attestation table
CREATE TABLE spdm_machine_devices_attestation (
    machine_id VARCHAR NOT NULL,
    device_id VARCHAR NOT NULL,
    nonce UUID NOT NULL,
    state JSONB,
    -- last_known_metadata will be swapped with 
    -- current_metadata after appraisal policy validation.
    last_known_metadata JSONB,
    current_metadata JSONB,
    ca_certificate_link VARCHAR,
    ca_certificate JSONB,
    evidence_target VARCHAR,
    evidence JSONB,
    PRIMARY KEY (machine_id, device_id),
    CONSTRAINT fk_machine_id
        FOREIGN KEY (machine_id)
        REFERENCES machines(id)
        ON DELETE CASCADE  -- force-delete handling.
        ON UPDATE CASCADE  -- Update predicted host id to permanent host id scenario.
);

-- Create the spdm_machine_attestation_history table
-- Updates on each state update (including device)
CREATE TABLE spdm_machine_attestation_history (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    machine_id VARCHAR NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    state_snapshot JSONB NOT NULL,  -- This field contains a hashmap for all components, evidences and certificates, and machine state. 
    state_version VARCHAR NOT NULL,
    attestation_status spdm_attestation_status_t NOT NULL
);
