-- The managed-host snapshot queries and the power manager look up
-- power_options by host_id; the table had no index, so every lookup scanned it.
CREATE INDEX power_options_host_id_idx
    ON power_options (host_id);
