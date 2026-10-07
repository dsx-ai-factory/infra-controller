-- Updates the Rack ID in expected machines to value that is specified in the RackIdentifier label
update expected_machines set rack_id=metadata_labels->>'RackIdentifier' where rack_id IS NULL AND metadata_labels->>'RackIdentifier' is not NULL;
