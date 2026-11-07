# Codebase overview

The repository is organized as a Rust workspace plus deployment, documentation, and development support directories.

`bluefield/` - `dpu-agent` and other tools running on the DPU.

`crates/` - source code for the Rust crates that make up NICo. Important crates include:

- `admin-cli/` - `nico-admin-cli`, a command line client for the nico API server.
- `api/` - the `nico-api` binary and NICo primary entrypoint for gRPC/API calls. It wires together and starts the core background modules and state controllers.
- `site-explorer/` - Site Explorer implementation. This code is compiled into and run by the `nico-api` binary.
- `preingestion-manager/` - Preingestion Manager implementation. This code is compiled into and run by the `nico-api` binary.
- `nvlink-manager/` - NVLink Manager / `NvlPartitionMonitor` implementation. This code is compiled into and run by the `nico-api` binary.
- `scout/` - `nico-scout`, a binary that runs on NICo managed hosts and DPUs and executes workflows on behalf of the site controller.
- `dhcp/` - Kea DHCP integration. It intercepts `DHCPDISCOVER`s from DHCP relays and forwards the information to `nico-api`.
- `dhcp-server/` - DHCP server written in Rust. This server runs on the DPU and serves host DHCP requests.
- `dns/` - DNS resolution for assets in the NICo database.
- `log-parser/` - service which parses SSH console logs and generates health alerts based on them.
- `pxe/` - `nico-pxe`, a web service which provides iPXE and cloud-init data to machines.
- `rpc/` - protobuf definitions and a Rust library for marshalling data between gRPC and native Rust types.

`dev/` - support files that are not product code, such as Dockerfiles, Kubernetes YAML, and local development helpers.

`docs/` - product documentation used by the documentation site.

`include/` - additional makefiles used by `cargo make`, as specified in `Makefile.toml`.
