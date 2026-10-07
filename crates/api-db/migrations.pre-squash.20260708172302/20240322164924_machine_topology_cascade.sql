-- Add migration script here
ALTER TABLE machine_topologies
    DROP CONSTRAINT machine_topologies_machine_id_fkey,
    ADD CONSTRAINT machine_topologies_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES machines(id)
        ON UPDATE CASCADE
;
