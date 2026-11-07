ALTER TABLE machine_validation_run_items
    ADD COLUMN plugin JSONB,
    ADD COLUMN plugin_full_host_approved BOOLEAN NOT NULL DEFAULT FALSE;
