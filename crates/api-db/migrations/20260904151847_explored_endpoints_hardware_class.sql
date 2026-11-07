-- Record the hardware an endpoint was explored as, so attestation can later
-- look up a profile by it without reading the exploration report. NULL until
-- the endpoint is explored again.
ALTER TABLE explored_endpoints
    ADD COLUMN hardware_class TEXT;
