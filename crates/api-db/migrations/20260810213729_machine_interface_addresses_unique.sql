-- A machine-interface address identifies one interface across the site. Normalize
-- legacy inet masks before enforcing that ownership in the database.
-- Keep one lock across both steps so older API processes cannot insert another
-- conflicting value in between. Existing duplicate owners intentionally stop
-- the migration instead of letting the migration choose which owner to keep.
LOCK TABLE public.machine_interface_addresses IN ACCESS EXCLUSIVE MODE;

UPDATE public.machine_interface_addresses
SET address = host(address)::inet
WHERE address != host(address)::inet;

ALTER TABLE public.machine_interface_addresses
    ADD CONSTRAINT machine_interface_addresses_host_address_check
    CHECK (address = host(address)::inet);

ALTER TABLE public.machine_interface_addresses
    ADD CONSTRAINT machine_interface_addresses_address_key UNIQUE (address);

DROP INDEX public.machine_interface_addresses_address_idx;
