-- Add a flag which allows to re-explore an endpoint
ALTER TABLE explored_endpoints
  ADD COLUMN exploration_requested bool NOT NULL DEFAULT false;
