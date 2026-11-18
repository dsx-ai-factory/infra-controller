-- The rack controller lists a rack's machines by rack_id on every handling;
-- the column had no index.
CREATE INDEX machines_rack_id_idx
    ON machines (rack_id);
