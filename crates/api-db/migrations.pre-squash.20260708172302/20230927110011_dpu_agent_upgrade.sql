
-- The active policy is the most recently created one
-- 'policy' is not an enum type to make adding policies easier
CREATE TABLE dpu_agent_upgrade_policy (
	policy varchar(32) NOT NULL DEFAULT 'Off',
	created TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE machines ADD COLUMN dpu_agent_upgrade_requested JSONB;
