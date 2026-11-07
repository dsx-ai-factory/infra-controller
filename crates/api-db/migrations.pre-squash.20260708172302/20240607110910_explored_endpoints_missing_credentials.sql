--- We REALLY shouldn't be serializing a Rust error directly into a database JSON field
UPDATE explored_endpoints SET exploration_report = '{"EndpointType": "Unknown", "LastExplorationError": {"Type": "MissingCredentials", "key": "blank", "cause": "blank"}}' WHERE exploration_report = '{"EndpointType": "Unknown", "LastExplorationError": {"Type": "MissingCredentials"}}';
