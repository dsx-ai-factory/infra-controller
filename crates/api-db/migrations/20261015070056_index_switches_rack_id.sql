-- The NVLink partition monitor publishes a rack's NMX-C domain on its
-- switches by rack_id every iteration; the column had no index.
CREATE INDEX switches_rack_id_idx
    ON switches (rack_id);
