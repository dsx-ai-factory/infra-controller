CREATE TABLE explored_managed_hosts(
    -- The IP of the Host BMC
    host_bmc_ip inet NOT NULL,
    -- The IP of the DPU BMC
    dpu_bmc_ip inet NOT NULL
);