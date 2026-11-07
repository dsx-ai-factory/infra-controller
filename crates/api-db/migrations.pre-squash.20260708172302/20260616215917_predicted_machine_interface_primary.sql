-- Carry the declared ExpectedHostNic.primary boot interface on the prediction so
-- it survives promotion into machine_interfaces (the predicted row previously
-- promoted as primary_interface = false unconditionally). Defaults false: a host
-- that declares nothing keeps today's automation -- the boot interface is chosen
-- by the pick_boot_interface fallback (lowest-MAC non-underlay) or DPU takeover.
ALTER TABLE predicted_machine_interfaces
    ADD COLUMN primary_interface boolean NOT NULL DEFAULT false;

-- The machine_id foreign key has no backing index (Postgres does not create one
-- for FK columns), so find_by_machine_id scans the table. Add it now that
-- promotion reads predictions by machine more often.
CREATE INDEX predicted_machine_interfaces_machine_id_idx
    ON predicted_machine_interfaces (machine_id);
