# `nico-admin-cli` reference

`nico-admin-cli` is the command-line tool for managing a NICo site. It
communicates with `nico-api` over gRPC with mutual TLS (mTLS).

Use the [NICo Admin CLI guide](../nico-admin-cli.md) to build the CLI,
configure TLS and logging, and test the connection to `nico-api`.
The [mTLS and authorization guide](../nico-api-auth.md) explains how to manage
client certificates and configure authentication on the API server.

## Command reference

Choose a domain to browse its commands.

| Domain | What you can manage |
|--------|----------|
| [Hardware](./hardware.md) | Machines, BMCs, DPUs, firmware, attestation, Redfish, and MLX devices |
| [Network](./network.md) | VPCs, peerings, prefixes, segments, security groups, InfiniBand and NVLink partitions, and IP address and domain lookups |
| [Tenant](./tenant.md) | Tenants, instances, compute allocations, expected inventory, OS images, iPXE templates, and extension services |
| [Admin](./admin.md) | CLI and system utilities |
