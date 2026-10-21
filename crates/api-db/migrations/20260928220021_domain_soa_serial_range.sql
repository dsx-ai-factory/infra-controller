-- SOA serials are u32 on the wire and in Rust. A stored value outside that
-- range would make the row undecodable, so reject it at write time instead.
ALTER TABLE domains ADD CONSTRAINT domains_soa_serial_range_check
    CHECK ((soa->>'serial')::bigint BETWEEN 0 AND 4294967295);
