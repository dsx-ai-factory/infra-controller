-- Update MatchingSku state to include machine_validation_context.  
UPDATE machines SET controller_state='{"state": "bomvalidating", "bom_validating_state": { "MatchingSku": { "machine_validation_context": "Discovery" } }}'::json
  WHERE controller_state->>'bom_validating_state' = 'MatchingSku';
