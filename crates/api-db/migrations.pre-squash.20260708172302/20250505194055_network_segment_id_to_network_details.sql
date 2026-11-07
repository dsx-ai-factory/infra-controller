
-- Update the interface for any instance network config that has network_segment_id but not network_details

/*
    This breaks the array in network_config->interfaces into a set of records.

    If network_details is already defined for the interface, nothing changes.

    If not defined, then we create it using the network_segment_id value of
    the interface.
    
    Then we aggregate the interface records again, and put the final result in
    network_config->interfaces.
*/

UPDATE instances i
    SET network_config=jsonb_set(
        network_config,
        '{interfaces}',
        (
            select jsonb_agg(ba.value) from (
                SELECT
                    jsonb_set(ifc_ttable.value,'{network_details}',
                    coalesce(
                        ifc_ttable.value->>'network_details',
                        concat('{"NetworkSegment": "', ifc_ttable.value->>'network_segment_id' ,'"}'))::jsonb
                    ) as value
                FROM jsonb_array_elements(i.network_config #>'{interfaces}') as ifc_ttable
           ) as ba
        )
    );
