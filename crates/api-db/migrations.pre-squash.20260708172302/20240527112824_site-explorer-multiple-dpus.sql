DELETE FROM explored_managed_hosts;
ALTER TABLE explored_managed_hosts
    DROP COLUMN dpu_bmc_ip,
    DROP COLUMN host_pf_mac_address,
    ADD COLUMN explored_dpus jsonb;