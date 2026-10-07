-- Adda field to store the health report receive by forge-dpu-agent
ALTER TABLE machines ADD COLUMN dpu_agent_health_report jsonb;
