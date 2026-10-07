CREATE TABLE redfish_bmc_actions (
  request_id bigint GENERATED ALWAYS AS IDENTITY NOT NULL,
  requester text NOT NULL,
  approvers text[] DEFAULT array[]::text[] NOT NULL,
  approver_dates timestamp with time zone[] DEFAULT array[]::timestamp with time zone[] NOT NULL,
  -- use the board serials to confirm the ip addresses were not reassigned
  machine_ips text[],
  board_serials text[],
  target text NOT NULL,
  -- storing the friendly action name in addition to the target.
  action text NOT NULL,
  parameters text NOT NULL,
  applied_at timestamp with time zone,
  applier text,
  results jsonb[],

  PRIMARY KEY(request_id)
);

CREATE INDEX redfish_bmc_actions_machine_ip_idx
ON redfish_bmc_actions
USING GIN (
  machine_ips array_ops
);
