-- Add migration script here

CREATE TABLE bmc_machine_controller_lock
(
    id uuid DEFAULT gen_random_uuid() NOT NULL
);

CREATE TYPE bmc_machine_type_t AS ENUM ('dpu', 'host');

CREATE TABLE bmc_machine(
  id uuid DEFAULT gen_random_uuid() NOT NULL,

  machine_interface_id uuid NOT NULL,

  -- Bmc type:
  --   * Dpu
  --   * Host
  bmc_type bmc_machine_type_t NOT NULL,

  -- The state of BMC Machine:
  --   * Init: 0
  controller_state_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268'),
  controller_state         jsonb       NOT NULL DEFAULT ('{"state":"init"}'),

  PRIMARY KEY(id),
  FOREIGN KEY(machine_interface_id) REFERENCES machine_interfaces(id)
);