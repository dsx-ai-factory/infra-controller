# Redfish Workflow

NICo uses [DMTF Redfish](https://www.dmtf.org/standards/redfish) to discover, provision, and monitor bare-metal hosts and their DPUs through BMC (Baseboard Management Controller) interfaces. This document traces the end-to-end workflow from initial DHCP discovery through ongoing monitoring.

For the overall NICo architecture and component responsibilities, see [Overview and components](overview.md). The Site Explorer component described there is the primary consumer of Redfish APIs.

## Workflow Summary

```text
DHCP Request (BMC)
  → NICo DHCP (Kea hook)
    → NICo Core (gRPC discover_dhcp)
      → Site Explorer probes Redfish endpoint
        → Authenticates, collects inventory
          → Pairs DPUs to hosts via serial number matching
            → Provisioning:
               1. Set DPU boot to HTTP IPv4 UEFI
               2. Power cycle DPU via Redfish
               3. DPU PXE boots nico.efi
               4. BIOS config (SR-IOV, etc.)
               5. Set host boot order (DPU first)
               6. Power cycle host via Redfish
            → Ongoing monitoring:
               - Firmware inventory (periodic)
               - Sensor collection (60s interval)
               - Prometheus metric export
```

## 1. DHCP Discovery

When a BMC on a NICo-managed physical network sends a DHCP request, the NICo
DHCP server (a Kea hook plugin) captures it and forwards the discovery
information to NICo Core. Site Explorer scans conventional `Underlay` BMC
interfaces and BMC-typed interfaces on the supported shared `HostInband`
topology; ordinary host data interfaces on HostInband are not Redfish targets.
See [Shared HostInband for a Host BMC and Host OS](../provisioning/ip-and-network-configuration.md#15-shared-hostinband-for-a-host-bmc-and-host-os).

The Kea hook is implemented as a Rust library with C FFI bindings. When a DHCP packet arrives, the hook:

1. Extracts the MAC address, vendor class string, relay address, circuit ID, and remote ID from the DHCP packet
2. Builds a `Discovery` struct with these fields
3. Sends a gRPC `discover_dhcp()` request to NICo Core with the MAC and vendor string
4. Receives back a `Machine` response containing the network configuration (IP address, gateway, etc.) to return to the BMC

The vendor class string is parsed to identify the BMC type and capabilities. DHCP entries are tracked in the database by MAC address and associated with machine interfaces.

**Key files:**

- `crates/dhcp/src/discovery.rs` — `Discovery` struct and FFI entry points (`discovery_fetch_machine`)
- `crates/dhcp/src/machine.rs` — `Machine::try_fetch()` sends gRPC discovery request
- `crates/dhcp/src/vendor_class.rs` — Vendor class parsing and BMC type identification
- `crates/api-model/src/dhcp_entry.rs` — `DhcpEntry` database model

## 2. Redfish Endpoint Probing and Inventory

Once NICo knows about a BMC IP from DHCP, the Site Explorer component continuously probes and inventories it via Redfish.

### Probing

Site Explorer first sends an anonymous (unauthenticated) GET to `/redfish/v1` (the Redfish service root) to detect the BMC vendor. The `RedfishVendor` enum identifies the vendor from the service root response, which determines vendor-specific behavior for subsequent operations.

### Authentication

After vendor detection, Site Explorer creates an authenticated Redfish session using one of three methods:

- **Anonymous** — Used for initial probing only
- **Direct** — Username/password from the Expected Machines manifest (factory defaults)
- **Key** — Credential key lookup by BMC MAC address (after credential rotation)

### Inventory Collection

With an authenticated session, Site Explorer queries a comprehensive set of Redfish resources and produces an `EndpointExplorationReport` containing:

| Data Collected | Redfish Source | Purpose |
|---|---|---|
| System serial numbers | `GET /redfish/v1/Systems/{id}` | Machine identification |
| Chassis serial numbers | `GET /redfish/v1/Chassis/{id}` | Fallback identification |
| Network adapters + serials | `GET /redfish/v1/Chassis/{id}/NetworkAdapters` | DPU-host pairing |
| PCIe devices + serials | `GET /redfish/v1/Systems/{id}` (PCIeDevices) | DPU-host pairing |
| Manager info | `GET /redfish/v1/Managers/{id}` | BMC firmware version |
| Ethernet interfaces | `GET /redfish/v1/Managers/{id}/EthernetInterfaces` | BMC network info |
| Firmware versions | `GET /redfish/v1/UpdateService/FirmwareInventory` | Version tracking |
| Boot configuration | `GET /redfish/v1/Systems/{id}/BootOptions` | Boot order state |
| Power state | `GET /redfish/v1/Systems/{id}` (PowerState) | Current state |

Serial numbers are trimmed of whitespace. If `system.serial_number` is missing, the chassis serial number is used as a fallback.

**Key files:**

- `crates/site-explorer/src/redfish.rs` — `RedfishClient`: `get_redfish_vendor()`, `create_redfish_client()`, inventory queries
- `crates/site-explorer/src/bmc_endpoint_explorer.rs` — `BmcEndpointExplorer` orchestrates credential lookup and exploration
- `crates/api-model/src/bmc_info.rs` — `BmcInfo` model (IP, port, MAC, firmware version)

## 3. DPU-Host Pairing

Once Site Explorer has explored both host BMCs and DPU BMCs, it matches them into host-DPU pairs using serial number correlation. This is the core logic that answers: "which DPU belongs to which host?"

### Matching Algorithm

The algorithm has three strategies, tried in order:

**Step 1 — Build DPU serial number map:**
For each explored DPU endpoint, extract `system.serial_number` and create a map: `DPU serial → explored endpoint`.

**Step 2 — Primary match via PCIe devices:**
For each host, iterate through `system.pcie_devices`. For each device where `is_bluefield()` returns true (BF2, BF3, or BF3 Super NIC), look up `pcie_device.serial_number` in the DPU serial map. A match means this DPU is physically installed in this host.

**Step 3 — Fallback match via chassis network adapters:**
If no BlueField PCIe devices were found (Step 2 count = 0), iterate through `chassis.network_adapters` instead. For each adapter where `is_bluefield_model(part_number)` is true, look up `network_adapter.serial_number` in the DPU serial map.

**Step 4 — Final fallback via expected machines manifest:**
If the explored matches are incomplete, check `expected_machine.fallback_dpu_serial_numbers` for manually specified DPU-to-host associations.

### Validation

Before accepting a pairing, NICo validates:

- **DPU mode**: The DPU must be in DPU mode, not NIC mode. BlueFields in NIC mode are excluded from pairing.
- **DPU model configuration**: `check_and_configure_dpu_mode()` verifies the DPU is correctly configured for its model. Hosts with misconfigured DPUs are not ingested.
- **Completeness**: The number of explored DPUs must match the number of BlueField devices the host reports. Incomplete pairings are deferred.

### Ingestion

Once all DPUs are matched and validated, the host enters an "ingestable" state and Site Explorer kickstarts the ingestion process via the ManagedHost state machine.

**Key file:**

- `crates/site-explorer/src/lib.rs`: `identify_managed_hosts()` with the complete pairing algorithm

## 4. DPU Provisioning

After pairing, the DPU must be provisioned with NICo software. This is orchestrated via Temporal workflows (in `nico-rest`) with Redfish power control (in `infra-controller`).

### Boot Configuration

The DPU is configured to boot from HTTP IPv4 UEFI, which directs it to the NICo PXE server. The PXE server serves different artifacts based on architecture:

- **ARM (BlueField DPUs)**: `nico.efi` with cloud-init user-data containing `machine_id` and `server_uri`
- **x86 (Hosts)**: `scout.efi` with machine discovery parameters (`cli_cmd=auto-detect`)

### Power Cycle

The DPU is power-cycled via Redfish to trigger the network boot:

```http
POST /redfish/v1/Systems/{system_id}/Actions/ComputerSystem.Reset
Body: {"ResetType": "GracefulRestart"}
```

The power control operation supports multiple reset types: `On`, `ForceOff`, `GracefulShutdown`, `GracefulRestart`, `ForceRestart`, `ACPowercycle`, `PowerCycle`.

### Installation

After PXE boot, the DPU:

1. Fetches `nico.efi` from the NICo PXE server over HTTP
2. Receives cloud-init configuration with its `machine_id` and NICo API endpoint
3. Installs and starts the DPU agent (`dpu-agent`), which connects back to NICo Core via gRPC

**Key files:**

- `crates/api/src/ipxe.rs` — iPXE instruction generation per architecture
- `pxe/ipxe/local/embed.ipxe` — iPXE boot script template
- `nico-rest/workflow/pkg/workflow/instance/reboot.go` — `RebootInstance` Temporal workflow
- `nico-rest/site-workflow/pkg/grpc/client/instance_powercycle.go` — Power cycle gRPC call to site agent

## 5. Host Configuration and Boot

With the DPU provisioned, NICo configures the host BIOS and boot order via Redfish.

### BIOS Attribute Setting

NICo sets BIOS attributes required for bare-metal infrastructure operation. This includes SR-IOV enablement and other platform-specific settings. BIOS operations use the libredfish `Redfish` trait:

- `bios()` — Read current BIOS attributes
- `set_bios()` — Set BIOS attribute values
- `machine_setup()` — Apply infrastructure-specific BIOS configuration
- `is_bios_setup()` / `machine_setup_status()` — Check configuration state

These translate to Redfish calls:

```http
GET  /redfish/v1/Systems/{id}/Bios           — Read attributes
PATCH /redfish/v1/Systems/{id}/Bios/Settings — Write attributes (pending next reboot)
```

### Boot Order Configuration

The host boot order is set so the DPU's network interface is the primary boot device:

```rust
set_boot_order_dpu_first(bmc_ip, credentials, boot_interface_mac)
```

This configures the UEFI boot order to prioritize the DPU's PF MAC address, ensuring the host boots through the DPU's network path.

### Host Reboot

After BIOS and boot order changes, the host is power-cycled via Redfish to apply the configuration:

```http
POST /redfish/v1/Systems/{system_id}/Actions/ComputerSystem.Reset
Body: {"ResetType": "GracefulRestart"}
```

Power cycles are rate-limited to avoid excessive reboots (checked via `time_since_redfish_powercycle` against `config.reset_rate_limit`).

**Key files:**

- `crates/site-explorer/src/redfish.rs` — `set_boot_order_dpu_first()`, `redfish_powercycle()`
- `crates/site-explorer/src/bmc_endpoint_explorer.rs` — Orchestrates boot order with credential lookup

## 6. Ongoing Monitoring

Once hosts are provisioned, the `nico-hw-health` service continuously monitors **both host BMCs and DPU BMCs** via Redfish. The endpoint discovery calls `find_machine_ids` with `include_dpus: true`, so every BMC known to NICo (host and DPU) gets its own set of collectors:

- **Health monitor** — sensor collection and health alert reporting
- **Firmware collector** — firmware inventory polling
- **Logs collector** — BMC event log collection

Each collector runs independently per BMC endpoint, meaning a host with two DPUs will have three sets of collectors (one for the host BMC, one for each DPU BMC).

### Firmware Inventory

The `FirmwareCollector` periodically queries each BMC's firmware inventory using **nv-redfish**:

```rust
let service_root = ServiceRoot::new(bmc.clone()).await?;
let update_service = service_root.update_service().await?;
let firmware_inventories = update_service.firmware_inventories().await?;
```

This translates to:

```http
GET /redfish/v1
GET /redfish/v1/UpdateService
GET /redfish/v1/UpdateService/FirmwareInventory
GET /redfish/v1/UpdateService/FirmwareInventory/{id}  (for each item)
```

Each firmware item's name and version is exported as a Prometheus gauge metric with labels:

- `serial_number` — Machine chassis serial
- `machine_id` — NICo machine UUID
- `bmc_mac` — BMC MAC address
- `firmware_name` — Component name (e.g., "BMC_Firmware", "DPU_NIC")
- `version` — Firmware version string

### Sensor Collection

Sensors (temperature, fan speed, power consumption, current draw) are collected at configurable intervals:

| Config Parameter | Default | Description |
|---|---|---|
| `sensor_fetch_interval` | 60 seconds | How often sensors are polled |
| `include_sensor_thresholds` | true | Whether to include threshold values |

Sensor, entity discovery, entity metrics, and SSE event-record fetches use the
global `bmc_request_concurrency` limit. It defaults to four concurrent Redfish
operations per BMC.

Sensor data is read from:

```http
GET /redfish/v1/Chassis/{id}/Sensors
GET /redfish/v1/Chassis/{id}/Sensors/{sensor_id}
```

Sensor types include: Temperature (Cel), Rotational/Fan (RPM), Power (W), and Current (A).

Sensor measurements are exported as Prometheus metrics on `/telemetry` on port 9009. Health reports derived from collected hardware events are submitted separately to NICo Core through `InsertMachineHealthReport` for health aggregation.

**Key files:**

- `crates/health/src/firmware_collector.rs` — `FirmwareCollector` using nv-redfish
- `crates/health/src/discovery.rs` — Creates and manages collectors per endpoint
- `crates/health/src/config.rs` — Polling intervals and concurrency configuration

## Redfish Libraries

NICo uses two Redfish client libraries concurrently. **nv-redfish** is replacing **libredfish** over time. Versions are pinned in the workspace dependencies in `Cargo.toml`.

| Library | Version | Language | Used For | Location in Code |
|---|---|---|---|---|
| [libredfish](https://github.com/NVIDIA/libredfish) | v0.43.11 | Rust | Site Explorer: discovery, boot config, power control, BIOS, account management | `crates/site-explorer/`, via `crates/redfish/` |
| [nv-redfish](https://github.com/NVIDIA/nv-redfish) | 0.7.1 | Rust | Site Explorer exploration and hardware health inventory collection | `crates/site-explorer/`, `crates/redfish/`, `crates/health/src/` |

**libredfish** provides a `Redfish` trait with vendor-specific implementations (Dell, HPE, Lenovo, Supermicro, NVIDIA DPU/GB200/GH200/Viking). It handles the full breadth of BMC operations.

**nv-redfish** uses a code-generation approach: CSDL (Redfish schema XML) is compiled into strongly-typed Rust at build time. It is feature-gated so only needed Redfish services are compiled in. Currently enabled features in NICo: `std-redfish`, `update-service`, `resource-status`.

Both libraries are declared in the workspace `Cargo.toml`.

## Redfish Endpoints Reference

For the complete list of Redfish endpoints and their required response fields,
see [Redfish Endpoints Reference](redfish/endpoints_reference.md).
