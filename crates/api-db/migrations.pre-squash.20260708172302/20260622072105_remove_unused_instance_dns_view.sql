-- Remove the unused instance-DNS view and its hostname-derivation function.
--
-- `dns_records_instance` derived forward A/AAAA names directly from instance IP
-- addresses via nico_inet_to_dns_hostname(). It was dropped from the served
-- `dns_records` view in 20250113220120 and has not been served since. IP-derived
-- hostnames are produced by the host-naming strategy (the Rust
-- `address_to_hostname` helper) and stored in machine_interfaces.hostname, which
-- the shortname/adm views serve -- so this view and function are a stale SQL
-- duplicate of that logic, with no remaining consumers.
DROP VIEW IF EXISTS dns_records_instance;
DROP FUNCTION IF EXISTS nico_inet_to_dns_hostname(inet);
