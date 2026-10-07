-- Add migration script here
CREATE TYPE network_segment_type_t AS ENUM ('tenant', 'admin', 'underlay');

ALTER TABLE network_segments
  DROP COLUMN admin_network,
  ADD COLUMN network_segment_type network_segment_type_t NOT NULL DEFAULT 'tenant'
;

CREATE UNIQUE INDEX only_one_admin_network_segment ON network_segments (network_segment_type) WHERE network_segment_type = 'admin';
