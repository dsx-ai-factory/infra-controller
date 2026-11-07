-- Per-host control over how a BMC's IP is assigned and whether it is retained.
-- See the `ExpectedMachine.bmc_ip_allocation` field and `BmcIpAllocationType`
-- enum in `forge.proto`. The default `auto` infers `fixed` from a configured
-- `bmc_ip_address` and otherwise `retained` (an auto-allocated address pinned
-- as Static so it survives DHCP lease expiry).
CREATE TYPE bmc_ip_allocation_t AS ENUM ('auto', 'dynamic', 'fixed', 'retained');

ALTER TABLE expected_machines
  ADD COLUMN bmc_ip_allocation bmc_ip_allocation_t NOT NULL DEFAULT 'auto';
