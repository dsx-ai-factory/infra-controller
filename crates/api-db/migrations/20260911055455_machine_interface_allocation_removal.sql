-- Remember removed stateful allocations without freezing either family's template.
ALTER TABLE machine_interfaces
    ADD COLUMN ipv4_allocation_removed BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN ipv6_allocation_removed BOOLEAN NOT NULL DEFAULT FALSE;
