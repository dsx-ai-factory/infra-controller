-- Same lookup for power shelves; one index per migration, refer to
-- 20261015070056_index_switches_rack_id.sql.
CREATE INDEX power_shelves_rack_id_idx
    ON power_shelves (rack_id);
