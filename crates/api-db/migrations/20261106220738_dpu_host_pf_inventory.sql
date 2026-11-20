-- Preserve agent-authenticated PF mappings across discovery JSON replacement by older writers.
ALTER TABLE machine_topologies ADD COLUMN dpu_host_pf_inventory jsonb;
