CREATE TABLE preingestion_manager_lock (
    id uuid DEFAULT gen_random_uuid() NOT NULL
);

ALTER TABLE explored_endpoints
    ADD COLUMN preingestion_state jsonb NOT NULL DEFAULT '{"state":"initial"}',
    ADD COLUMN waiting_for_explorer_refresh bool NOT NULL DEFAULT false;
