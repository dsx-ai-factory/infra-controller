-- Migrate machines stuck in the removed VerifyRmsMembership or
-- RegisterRmsMembership states to DpuDiscoveringState with all attached
-- DPUs set to Initializing.
--
-- The controller_state column stores a tagged JSON enum (serde
-- rename_all = "lowercase"), so the old values look like:
--   {"state":"verifyrmsmembership"}
--   {"state":"registerrmsmembership"}
--
-- The target state is DpuDiscoveringState whose serialised form is:
--   {"state":"dpudiscoveringstate","dpu_states":{"states":{<id>:{"dpudiscoverystate":"initializing"}, ...}}}
--
-- For zero-DPU hosts the states map will be empty, which matches the
-- code path in the state handler (it immediately skips to HostInit).

UPDATE machines m
SET    controller_state = jsonb_build_object(
           'state', 'dpudiscoveringstate',
           'dpu_states', jsonb_build_object(
               'states', COALESCE(
                   (SELECT jsonb_object_agg(
                               mi.attached_dpu_machine_id,
                               '{"dpudiscoverystate":"initializing"}'::jsonb
                           )
                    FROM   machine_interfaces mi
                    WHERE  mi.machine_id = m.id
                      AND  mi.attached_dpu_machine_id IS NOT NULL
                   ),
                   '{}'::jsonb
               )
           )
       )
WHERE  m.controller_state ->> 'state' IN (
           'verifyrmsmembership',
           'registerrmsmembership'
       );
