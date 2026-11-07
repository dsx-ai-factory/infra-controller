ALTER TABLE network_segments
    ADD COLUMN can_stretch bool DEFAULT NULL
;

-- Any existing network segments belonging to legacy VPCs should have this flag
-- set to true, since that's the only behavior supported by the ETV VPC type.
UPDATE network_segments SET can_stretch = true WHERE id IN (
    SELECT ns.id FROM network_segments ns INNER JOIN vpcs v ON ns.vpc_id = v.id
    WHERE v.network_virtualization_type = 'etv'
    AND ns.network_segment_type = 'tenant'
);
