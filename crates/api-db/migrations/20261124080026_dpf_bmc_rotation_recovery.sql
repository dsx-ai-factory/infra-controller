CREATE TABLE dpf_bmc_rotation_recovery (
    device_mac macaddr PRIMARY KEY,
    operation_id uuid NOT NULL,
    host_machine_id text NOT NULL,
    host_dpf_id text NOT NULL,
    previous_version integer CHECK (previous_version >= 0),
    target_version integer NOT NULL CHECK (target_version >= 0),
    patch_pending boolean NOT NULL,
    detached boolean NOT NULL
);
