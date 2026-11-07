-- After enough issues with UpOnly not syncing agent versions, we've decided UpDown is what we want
-- everywhere.
UPDATE dpu_agent_upgrade_policy SET policy = 'UpDown' WHERE policy = 'UpOnly';