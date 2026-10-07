-- Update controller state for machine validation states
UPDATE machines
SET controller_state = jsonb_build_object(
    'state', 'validation',
    'validation_state', jsonb_build_object(
        'validation_type', 'machinevalidation',
        'machine_validation', jsonb_build_object(
            'machinevalidating', jsonb_build_object(
                'id', controller_state->'machine_state'->>'id',
                'total', (controller_state->'machine_state'->>'total')::int,
                'context', controller_state->'machine_state'->>'context',
                'completed', (controller_state->'machine_state'->>'completed')::int,
                'is_enabled', (controller_state->'machine_state'->>'is_enabled')::boolean
            )
        )
    )
)
WHERE controller_state->>'state' = 'hostinit'
AND controller_state->'machine_state'->>'state' = 'machinevalidating'; 