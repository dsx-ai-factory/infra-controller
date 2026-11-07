-- Fixes the infiniband status observation
-- The date format was off, and `config_version` had an `ib_prefix` that isn't used.
ALTER TABLE instances
    ALTER COLUMN ib_status_observation SET DEFAULT ('{"observed_at": "2023-01-01T00:00:00.000000000Z", "config_version": "V1-T1666644937952267"}')
;

-- Since resetting the default doesn't apply to existing entries, we have to patch those too
UPDATE instances
  SET ib_status_observation = '{"observed_at": "2023-01-01T00:00:00.000000000Z", "config_version": "V1-T1666644937952267"}'
  WHERE ib_status_observation = '{"ib_config_version": "V1-T1666644937952267", "observed_at": "2023-01-01 00:00:00.000000+00"}';
