-- Add allocation_strategy to network segments.
-- "dynamic" (default): DHCP allocator hands out IPs from the pool.
-- "reserved": Only pre-existing static/fixed-address reservations are served.
ALTER TABLE network_segments
    ADD COLUMN allocation_strategy TEXT NOT NULL DEFAULT 'dynamic';
