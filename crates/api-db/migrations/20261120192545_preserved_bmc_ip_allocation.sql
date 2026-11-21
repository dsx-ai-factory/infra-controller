-- Keep the legacy Host BMC policy column in sync with the nested interface policy.
ALTER TYPE bmc_ip_allocation_t ADD VALUE 'preserved';
