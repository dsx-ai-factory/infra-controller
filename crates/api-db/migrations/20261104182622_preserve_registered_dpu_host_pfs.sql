-- Keep registered host-PF facts when older APIs replace discovery_data without
-- host_pfs. Preserve their part number too, so the replacement cannot erase the
-- BF4 classification that makes the registered inventory required.
CREATE FUNCTION preserve_registered_dpu_host_pfs()
RETURNS TRIGGER AS $$
DECLARE
    registered jsonb := OLD.topology #> '{discovery_data,Info,dpu_info}';
    refreshed jsonb := NEW.topology #> '{discovery_data,Info,dpu_info}';
BEGIN
    IF jsonb_typeof(registered -> 'host_pfs') = 'array'
       AND registered -> 'host_pfs' <> '[]'::jsonb
       AND COALESCE(refreshed -> 'host_pfs', 'null'::jsonb)
           IN ('null'::jsonb, '[]'::jsonb) THEN
        -- Missing DpuData must retain the registered facts without discarding
        -- unrelated discovery fields supplied by the refreshing API.
        IF jsonb_typeof(refreshed) IS DISTINCT FROM 'object' THEN
            refreshed := registered;
        END IF;
        refreshed := refreshed || jsonb_build_object('host_pfs', registered -> 'host_pfs');
        -- Preserve absence when the registered part number was not serialized.
        IF registered ? 'part_number' THEN
            refreshed := jsonb_set(refreshed, '{part_number}', registered -> 'part_number');
        END IF;
        NEW.topology := jsonb_set(
            NEW.topology,
            '{discovery_data,Info,dpu_info}',
            refreshed,
            true
        );
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER preserve_registered_dpu_host_pfs
BEFORE UPDATE OF topology ON machine_topologies
FOR EACH ROW
EXECUTE FUNCTION preserve_registered_dpu_host_pfs();
