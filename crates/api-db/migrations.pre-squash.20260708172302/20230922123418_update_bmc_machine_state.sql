--- Update default BMC state

ALTER TABLE  bmc_machine ALTER COLUMN controller_state SET DEFAULT '{"state":"initializing"}';

UPDATE bmc_machine SET controller_state=jsonb_set(controller_state, '{state}', to_jsonb('initializing'::text)) WHERE controller_state->> 'state' = 'init' OR controller_state->> 'state' = 'redfishconnection'