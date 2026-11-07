-- Existing and new segments start with SLAAC EUI-64 inference disabled.
-- Existing address rows remain unchanged; this setting gates future inferred rows.
ALTER TABLE network_segments
    ADD COLUMN infer_slaac_eui64_addresses BOOLEAN NOT NULL DEFAULT false;
