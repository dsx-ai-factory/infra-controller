--
-- FOR LOCAL DEVELOPMENT ONLY
--
-- Delete everything that `cargo make bootstrap-forge-docker` puts in the database, so that we
-- can re-run it without a full env restart.
--
-- Handy SQL to select * from all tables:
--  SELECT schemaname,relname,n_live_tup FROM pg_stat_user_tables ORDER BY n_live_tup DESC;
--
-- Usage: PGPASSWORD=<thing> psql -h 172.20.0.16 -U carbide_development < cleanup_bootstrap.sql

DELETE FROM instance_addresses;
DELETE FROM instances;
DELETE FROM port_to_network_device_map;
DELETE FROM machine_topologies;
DELETE FROM machine_interface_addresses;
DELETE FROM machine_interfaces;
DELETE FROM machine_state_history;
DELETE FROM machines;
DELETE FROM explored_endpoints;
DELETE FROM explored_managed_hosts;
DELETE FROM dhcp_entries;
