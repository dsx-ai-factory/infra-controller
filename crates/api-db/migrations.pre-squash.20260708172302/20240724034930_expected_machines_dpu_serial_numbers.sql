ALTER TABLE expected_machines
    ADD COLUMN fallback_dpu_serial_numbers text[] NOT NULL DEFAULT '{}'
;