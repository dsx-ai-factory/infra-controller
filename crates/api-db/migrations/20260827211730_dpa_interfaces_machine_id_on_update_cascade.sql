-- dpa_interfaces must follow a machines.id rename (predicted host id -> stable
-- host id) the way machine_interfaces already does. Without ON UPDATE CASCADE,
-- the UPDATE machines SET id fails while dpa_interfaces still references the old
-- id. Recreate the FK with the cascade; ON DELETE stays the default (deletes are
-- handled explicitly in Rust and the delete_machine SQL function).
ALTER TABLE public.dpa_interfaces
    DROP CONSTRAINT dpa_interfaces_machine_id_fkey,
    ADD CONSTRAINT dpa_interfaces_machine_id_fkey
        FOREIGN KEY (machine_id) REFERENCES public.machines(id) ON UPDATE CASCADE;
