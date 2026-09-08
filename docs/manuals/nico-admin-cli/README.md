# `nico-admin-cli` reference

`nico-admin-cli` is the command-line tool for managing a NICo site. It
communicates with `nico-api` over gRPC with mutual TLS (mTLS).

For building the CLI, connecting to `nico-api`, TLS flag reference, logging,
and a quick connectivity check, see [`nico-admin-cli.md`](../nico-admin-cli.md).
For mTLS cert generation and server-side auth configuration, see
[`nico-api-auth.md`](../nico-api-auth.md).

## Command reference

| Domain | Commands |
|--------|----------|
| [Hardware](./hardware.md) | Machines, BMC, DPUs, firmware, attestation, Redfish, RMS, MLX |
| [Network](./network.md) | VPCs, peerings, prefixes, segments, security groups, IB/NVLink, IP/domain lookups |
| [Tenant](./tenant.md) | Tenants, instances, compute allocations, expected-inventory, OS images, iPXE, extension services |
| [Admin](./admin.md) | CLI and system utilities |
