# Redfish Endpoints Reference

This page documents all Redfish endpoints used by NVIDIA Infra Controller (NICo), organized by resource group. Each section includes endpoint tables, required response fields with their importance to NICo, and vendor-specific notes.

Field importance levels:
- **Critical** — NICo cannot function correctly without this field. Pairing, identification, or core workflows fail.
- **Required** — Expected by NICo and used in normal operation. Missing values cause degraded behavior.
- **Recommended** — Used when available, with graceful fallback if absent.
- **Optional** — Informational or used only in specific configurations.

> For the manually-maintained tracker with full vendor coverage and response payload examples, see the [DSX OEM Redfish APIs spreadsheet](https://docs.google.com/spreadsheets/d/1iUz1mDv3pcylVgiawx8VBx9G-9vVKAzkMyeQAeGbc0U/edit?gid=0#gid=0).

---

## Service Root

**Code**: `get_service_root()` in libredfish; `get_redfish_vendor()` in `site_explorer/redfish.rs`

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1` | GET | Service root, vendor detection |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `Vendor` | Required | Vendor detection — determines all vendor-specific behavior |
| `Systems` | Required | Link to systems collection |
| `Managers` | Required | Link to managers collection |
| `Chassis` | Required | Link to chassis collection |
| `UpdateService` | Required | Link to firmware update service |

---

## Systems

**Code**: `get_systems()`, `get_system()` in libredfish; exploration through nv-redfish in `crates/bmc-explorer/`

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Systems` | GET | List computer systems |
| `/redfish/v1/Systems/{id}` | GET | System info, serial number, power state |
| `/redfish/v1/Systems/{id}` | PATCH | Boot source override (boot_once/boot_first) |
| `/redfish/v1/Systems/{id}/Actions/ComputerSystem.Reset` | POST | Power control (On/ForceOff/GracefulRestart/ForceRestart/ACPowercycle/PowerCycle) |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `SerialNumber` | **Critical** | Machine ID generation via DMI hash. Pairing fails without it. |
| `Id` | Required | DPU detection (checks for "bluefield" substring) |
| `PowerState` | Required | Health reporting, preingestion state validation. Values: On, Off, PoweringOn, PoweringOff, Paused, Reset |
| `Boot.BootOrder` | Required | Boot order reporting and verification |
| `Boot.BootOptions` | Required | Link to boot options for interface detection |
| `PCIeDevices` | Required | Array of links — primary DPU-host pairing path |
| `EthernetInterfaces` | Required | Link to system NICs for DPU pairing |
| `Model` | Recommended | DPU model detection (BF2 vs BF3). Falls back gracefully. |
| `Manufacturer` | Recommended | Machine ID generation. Has `DEFAULT_DMI_SYSTEM_MANUFACTURER` fallback. |
| `SKU` | Optional | Validation against expected machines manifest |
| `BiosVersion` | Optional | BIOS version tracking |
| `TrustedModules` | Optional | TPM status reporting |

**Sample response** (`GET /redfish/v1/Systems/{id}`):

```json
{
  "Id": "System.Embedded.1",
  "SerialNumber": "J1234XY",
  "PowerState": "On",
  "Manufacturer": "Dell Inc.",
  "Model": "PowerEdge R750",
  "Boot": {
    "BootOrder": ["NIC.Slot.3-1", "HardDisk.Direct.0-0:AHCI"],
    "BootOptions": { "@odata.id": "/redfish/v1/Systems/System.Embedded.1/BootOptions" }
  },
  "PCIeDevices": [
    { "@odata.id": "/redfish/v1/Systems/System.Embedded.1/PCIeDevices/236-0" }
  ],
  "EthernetInterfaces": { "@odata.id": "/redfish/v1/Systems/System.Embedded.1/EthernetInterfaces" }
}
```

**Vendor-specific notes**: Dell/Supermicro/HPE have system info overrides. NVIDIA DPU uses `Oem.Nvidia` for mode set/rshim. NVIDIA GBx00 uses `Oem.Nvidia` for machine setup.

---

## System Ethernet Interfaces

**Code**: `get_system_ethernet_interfaces()`, `get_system_ethernet_interface()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Systems/{id}/EthernetInterfaces` | GET | List system network interfaces |
| `/redfish/v1/Systems/{id}/EthernetInterfaces/{id}` | GET | Interface details (MAC, UEFI path) |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `MACAddress` (or `MacAddress`) | **Critical** | DPU-host pairing, interface identification. Accepts both field name variants. |
| `UefiDevicePath` | Required | Primary interface detection via PCI path ordering (parsed to format "2.1.0.0.0") |
| `Id` | Required | Interface identification |
| `InterfaceEnabled` | Optional | Error handling — disabled interfaces may have invalid MAC values |

---

## Chassis

**Code**: `get_chassis_all()`, `get_chassis()`, `get_chassis_assembly()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Chassis` | GET | List chassis |
| `/redfish/v1/Chassis/{id}` | GET | Chassis info, serial number |
| `/redfish/v1/Chassis/{id}/Assembly` | GET | Assembly info (GB200 serial extraction) |
| `/redfish/v1/Chassis/{id}/Actions/Chassis.Reset` | POST | Chassis power control (AC power cycle) |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `Id` | **Critical** | System classification: "Card1"=DPU, "powershelf"=power shelf, "mgx_nvswitch_0"=NVSwitch, "Chassis_0"=GB200 |
| `SerialNumber` | **Critical** | Fallback for system serial (DPU uses Chassis/Card1 serial). Power shelf/switch IDs. Whitespace trimmed. |
| `PartNumber` | Required | BlueField DPU identification via part number matching (900-9d3b6, SN37B36732, etc.) |
| `NetworkAdapters` | Required | Link to network adapters collection for DPU identification |
| `Model` | Recommended | Model identification. GB200: Assembly checked for "GB200 NVL" model. |
| `Manufacturer` | Recommended | Power shelf vendor identification. Has fallback defaults. |
| `Oem.Nvidia.chassis_physical_slot_number` | Optional | Physical slot in multi-node systems |
| `Oem.Nvidia.compute_tray_index` | Optional | Tray index in modular systems |
| `Oem.Nvidia.topology_id` | Optional | System topology identifier |

**Sample response** (`GET /redfish/v1/Chassis/{id}`):

```json
{
  "Id": "Card1",
  "SerialNumber": "MBF2M516A-CECA_Ax_SN123456",
  "PartNumber": "900-9D3B6-00CV-AA0",
  "Model": "BlueField-2 DPU 25GbE",
  "Manufacturer": "NVIDIA",
  "NetworkAdapters": { "@odata.id": "/redfish/v1/Chassis/Card1/NetworkAdapters" }
}
```

---

## Network Adapters

**Code**: `get_chassis_network_adapters()`, `get_chassis_network_adapter()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Chassis/{id}/NetworkAdapters` | GET | List network adapters |
| `/redfish/v1/Chassis/{id}/NetworkAdapters/{id}` | GET | Adapter details (serial, part number) |
| `/redfish/v1/Chassis/{id}/NetworkAdapters/{id}/NetworkDeviceFunctions` | GET | Network device functions (NVIDIA DPU) |
| `/redfish/v1/Chassis/{id}/NetworkAdapters/{id}/Ports` | GET | Network adapter ports |
| `/redfish/v1/Chassis/{id}/NetworkAdapters/{id}/Ports/{id}` | GET | Port details |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `SerialNumber` | **Critical** | DPU-host pairing fallback path. **Must be visible to Host BMC.** Whitespace trimmed. |
| `PartNumber` | **Critical** | BlueField/SuperNIC identification via `is_bluefield_model()` |
| `Id` | Required | Adapter tracking |

**Sample response** (`GET /redfish/v1/Chassis/{id}/NetworkAdapters/{id}`):

```json
{
  "Id": "ConnectX6_1",
  "SerialNumber": "MT2243X01234",
  "PartNumber": "MCX653106A-HDAT_Ax",
  "Controllers": [
    {
      "FirmwarePackageVersion": "24.37.1014",
      "Links": { "PCIeDevices": [{ "@odata.id": "/redfish/v1/Systems/System.Embedded.1/PCIeDevices/236-0" }] }
    }
  ]
}
```

---

## PCIe Devices

**Code**: `pcie_devices()` in libredfish; exploration through nv-redfish in `crates/bmc-explorer/`

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Chassis/{id}/PCIeDevices` | GET | PCIe device list (Supermicro uses chassis path) |
| `/redfish/v1/Chassis/{id}/PCIeDevices/{id}` | GET | PCIe device details |
| `/redfish/v1/Systems/{id}` (PCIeDevices array) | GET | PCIe device links embedded in system response |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `SerialNumber` | **Critical** | **Primary DPU-host pairing** — matched against DPU system serial numbers |
| `PartNumber` | **Critical** | BlueField identification via `is_bluefield_model()` (BF2, BF3, BF3 SuperNIC) |
| `Id` | Required | Device tracking |

**Vendor-specific note**: Supermicro uses `Chassis/{id}/PCIeDevices`; others embed PCIeDevices links in `Systems/{id}` response.

---

## Managers

**Code**: `get_managers()`, `get_manager()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Managers` | GET | List BMC managers |
| `/redfish/v1/Managers/{id}` | GET | BMC info, firmware version |
| `/redfish/v1/Managers/{id}/Actions/Manager.Reset` | POST | BMC reset |
| `/redfish/v1/Managers/{id}/Actions/Manager.ResetToDefaults` | POST | BMC factory reset |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `Id` | Required | Manager identification. Viking detection: `id == "BMC"`. Sets default manager ID for subsequent calls. |
| `FirmwareVersion` | Required | BMC firmware version tracking |
| `UUID` | Recommended | Manager unique identification |
| `EthernetInterfaces` | Required | Link to BMC network interfaces |
| `LogServices` | Required | Link to log services for event collection |

**Vendor-specific notes**: HPE has lockdown status override. Dell uses `Managers/{id}/Attributes` for lockdown/remote access. Supermicro uses `Oem/Supermicro/SysLockdown`.

---

## Manager Ethernet Interfaces

**Code**: `get_manager_ethernet_interfaces()`, `get_manager_ethernet_interface()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Managers/{id}/EthernetInterfaces` | GET | List BMC interfaces |
| `/redfish/v1/Managers/{id}/EthernetInterfaces/{id}` | GET | BMC MAC, IP configuration |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `MACAddress` | **Critical** | BMC identification and credential storage/lookup |

**Sample response** (`GET /redfish/v1/Managers/{id}/EthernetInterfaces/{id}`):

```json
{
  "Id": "1",
  "MACAddress": "B8:3F:D2:90:95:82",
  "IPv4Addresses": [{ "Address": "10.0.1.100" }]
}
```

---

## Boot Options

**Code**: `get_boot_options()`, `get_boot_option()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Systems/{id}/BootOptions` | GET | List boot options |
| `/redfish/v1/Systems/{id}/BootOptions/{id}` | GET | Boot option details |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `DisplayName` | Required | OOB interface detection (checks for "OOB" string) |
| `UefiDevicePath` | Required | MAC extraction via regex `MAC\((?<mac>[[:alnum:]]+)\,` — e.g. extracts `B83FD2909582` to `B8:3F:D2:90:95:82` |
| `BootOptionEnabled` | Optional | Boot option state |
| `BootOptionReference` | Required | Boot option ordering |

**Sample response** (`GET /redfish/v1/Systems/{id}/BootOptions/{id}`):

```json
{
  "Id": "NIC.Slot.3-1",
  "DisplayName": "PXE OOB NIC Slot 3 Port 1",
  "UefiDevicePath": "PciRoot(0x2)/Pci(0x1,0x0)/Pci(0x0,0x0)/MAC(B83FD2909582,0x1)",
  "BootOptionEnabled": true,
  "BootOptionReference": "NIC.Slot.3-1"
}
```

---

## BIOS

**Code**: `bios()`, `set_bios()`, `pending()`, `clear_pending()`, `reset_bios()`, `change_bios_password()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Systems/{id}/Bios` | GET | Read BIOS attributes |
| `/redfish/v1/Systems/{id}/Bios/Settings` | GET | Read pending BIOS changes |
| `/redfish/v1/Systems/{id}/Bios/Settings` | PATCH | Write BIOS attributes (pending next reboot) |
| `/redfish/v1/Systems/{id}/Bios/Actions/Bios.ResetBios` | POST | BIOS factory reset |
| `/redfish/v1/Systems/{id}/Bios/Actions/Bios.ChangePassword` | POST | UEFI password management |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `Attributes` | Required | BIOS attribute read/write (SR-IOV enablement, machine setup) |

**Vendor-specific paths**: HPE uses `/Bios/settings` (lowercase). Lenovo uses `/Bios/Pending`. Viking uses `/Bios/SD`. Dell/NVIDIA DPU/GBx00/Supermicro have attribute-specific overrides.

---

## Secure Boot

**Code**: `get_secure_boot()`, `enable_secure_boot()`, `disable_secure_boot()`, `get_secure_boot_certificates()`, `add_secure_boot_certificate()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Systems/{id}/SecureBoot` | GET | Read secure boot status |
| `/redfish/v1/Systems/{id}/SecureBoot` | PATCH | Enable/disable secure boot |
| `/redfish/v1/Systems/{id}/SecureBoot/SecureBootDatabases/{db}/Certificates` | GET | List secure boot certs |
| `/redfish/v1/Systems/{id}/SecureBoot/SecureBootDatabases/{db}/Certificates` | POST | Add secure boot cert |
| `/redfish/v1/Systems/{id}/SecureBoot/SecureBootDatabases/{db}/Certificates/{id}` | GET | Cert details |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `SecureBootEnable` | Required | Secure boot enabled status |
| `SecureBootCurrentBoot` | Required | Current boot secure boot state |
| `SecureBootMode` | Optional | Secure boot mode reporting |

---

## Account Service

**Code**: `get_accounts()`, `change_password_by_id()`, `create_user()`, `delete_user()`, `set_machine_password_policy()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/AccountService` | PATCH | Password policy/lockout settings |
| `/redfish/v1/AccountService/Accounts` | GET | List user accounts |
| `/redfish/v1/AccountService/Accounts` | POST | Create user account |
| `/redfish/v1/AccountService/Accounts/{id}` | GET | Account details |
| `/redfish/v1/AccountService/Accounts/{id}` | PATCH | Password/username change |
| `/redfish/v1/AccountService/Accounts/{id}` | DELETE | Delete user account |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `UserName` | Required | Account management |
| `Password` | Required | Credential rotation |
| `RoleId` | Required | Admin role verification |
| `Id` | Required | Account identification. Vendor-specific: Lenovo="1", AMI/Viking="2", NVIDIA=current user. |

---

## Firmware Inventory

**Code**: `get_software_inventories()`, `get_firmware()` in libredfish; `FirmwareCollector` in health crate via nv-redfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/UpdateService` | GET | Update service info |
| `/redfish/v1/UpdateService/FirmwareInventory` | GET | List firmware components |
| `/redfish/v1/UpdateService/FirmwareInventory/{id}` | GET | Component version details |
| `/redfish/v1/UpdateService/Actions/UpdateService.SimpleUpdate` | POST | URL-based firmware update |
| `/redfish/v1/UpdateService/MultipartUpload` | POST | Binary firmware upload (Dell) |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `Id` | Required | Component ID — matched against firmware config regex. Vendor-specific IDs: NVIDIA DPU=`DPU_NIC`/`DPU_UEFI`, Supermicro=`CPLD_Backplane_1`/`CPLD_Motherboard`, GBx00=`EROT_BIOS_0`/`HGX_FW_BMC_0`/`HostBMC_0` |
| `Version` | Required | Firmware version — used for upgrade decisions. DPU versions: trim, lowercase, remove "bf-" prefix. |
| `Name` | Required | Component name — exported as Prometheus metric label `firmware_name` |
| `ReleaseDate` | Optional | Informational |

**Sample response** (`GET /redfish/v1/UpdateService/FirmwareInventory/{id}`):

```json
{
  "Id": "BMC_Firmware",
  "Name": "BMC Firmware",
  "Version": "7.00.00.171",
  "ReleaseDate": "2024-06-15T00:00:00Z",
  "Updateable": true
}
```

---

## Sensors and Thermal (Health Monitoring)

**Code**: `monitor.rs` in health crate; `get_thermal_metrics()`, `get_power_metrics()` in libredfish

All endpoints below are polled at the configured `sensor_fetch_interval` (default 60 seconds).

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Chassis/{id}/Sensors` | GET | Environmental sensors |
| `/redfish/v1/Chassis/{id}/Thermal` | GET | Temperature/fan readings |
| `/redfish/v1/Chassis/{id}/Power` | GET | Power consumption/PSU |
| `/redfish/v1/Chassis/{id}/PowerSupplies` | GET | Power supply collection |
| `/redfish/v1/Chassis/{id}/PowerSupplies/{id}/Sensors` | GET | PSU sensor metrics |
| `/redfish/v1/Systems/{id}/Processors/{id}/EnvironmentSensors` | GET | CPU temperature |
| `/redfish/v1/Systems/{id}/Memory/{id}/EnvironmentSensors` | GET | Memory temperature |
| `/redfish/v1/Systems/{id}/Storage/{id}/Drives/{id}/EnvironmentSensors` | GET | Drive temperature |
| `/redfish/v1/Chassis/{id}/Drives` | GET | Drive info (GBx00) |
| `/redfish/v1/Chassis/{id}/ThermalSubsystem/ThermalMetrics` | GET | Thermal metrics (GBx00) |
| `/redfish/v1/Chassis/{id}/ThermalSubsystem/LeakDetection/LeakDetectors` | GET | Leak detection (GBx00) |
| `/redfish/v1/Chassis/{id}/EnvironmentMetrics` | GET | Chassis power (GBx00/DPS) |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `Reading` / `ReadingCelsius` | Required | Sensor value for Prometheus metrics |
| `ReadingUnits` / `ReadingType` | Required | Sensor classification: Cel, RPM, W, A |
| `Name` | Required | Sensor identification in Prometheus labels |
| `Status.Health` | Required | Health state: Ok, Warning, Critical |
| `Thresholds.UpperCritical` | Optional | Alert thresholds (configurable via `include_sensor_thresholds`) |
| `Thresholds.LowerCritical` | Optional | Alert thresholds |
| `ReadingRangeMax` / `ReadingRangeMin` | Optional | Valid reading range |

---

## Log Services

**Code**: `logs_collector.rs` in health crate; `get_bmc_event_log()`, `get_system_event_log()` in libredfish

Log collection runs at 5-minute intervals and uses incremental fetching: `?$filter=Id gt '{last_id}'`

### Discovery endpoints (all vendors)

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Managers/{id}/LogServices` | GET | Discover manager log services |
| `/redfish/v1/Chassis/{id}/LogServices` | GET | Chassis log services |
| `/redfish/v1/Systems/{id}/LogServices` | GET | System log services |

### BMC event log entries (vendor-specific)

| Endpoint | Method | Vendor |
|----------|--------|--------|
| `/redfish/v1/Managers/{id}/LogServices/Sel/Entries` | GET | Dell |
| `/redfish/v1/Managers/{id}/LogServices/IEL/Entries` | GET | HPE |
| `/redfish/v1/Managers/{id}/LogServices/SEL/Entries` | GET | Viking |
| `/redfish/v1/Systems/{id}/LogServices/AuditLog/Entries` | GET | Lenovo |

### System event log entries (vendor-specific)

| Endpoint | Method | Vendor |
|----------|--------|--------|
| `/redfish/v1/Systems/{id}/LogServices/EventLog/Entries` | GET | NVIDIA DPU |
| `/redfish/v1/Systems/{id}/LogServices/SEL/Entries` | GET | NVIDIA DPU/GBx00 |
| `/redfish/v1/Systems/{id}/LogServices/IML/Entries` | GET | HPE |

### Key Response Fields

| Field | Importance | NICo Usage |
|-------|-----------|---------------|
| `Id` | Required | Entry identifier for incremental collection |
| `Created` | Required | Timestamp |
| `Severity` | Required | Critical/Warning/Ok — maps to OTEL severity |
| `Message` | Required | Log message text |
| `MessageArgs` | Optional | Message format arguments |

---

## Task Service

**Code**: `get_tasks()`, `get_task()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/TaskService/Tasks` | GET | List async operation tasks |
| `/redfish/v1/TaskService/Tasks/{id}` | GET | Task status (firmware updates, lockdown, etc.) |

Dell also uses `Managers/{id}/Jobs/{id}` (converted to Task internally).

---

## Component Integrity

**Code**: `get_component_integrities()`, `get_component_ca_certificate()`, `trigger_evidence_collection()`, `get_evidence()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/ComponentIntegrity` | GET | SPDM attestation components |
| `{component}/Certificates/CertChain` | GET | Component CA certificate |
| `{component}/Actions/ComponentIntegrity.SPDMGetSignedMeasurements` | POST | Trigger evidence collection |

---

## Manager Network Protocol

**Code**: `get_manager_network_protocol()` in libredfish

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Managers/{id}/NetworkProtocol` | GET | BMC network services config |
| `/redfish/v1/Managers/{id}/NetworkProtocol` | PATCH | Enable/disable IPMI access |

---

## Storage

**Code**: `get_drives_metrics()` in libredfish; `discover_drive_entities()` in health monitor

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/redfish/v1/Systems/{id}/Storage` | GET | List storage controllers |
| `/redfish/v1/Systems/{id}/Storage/{id}` | GET | Storage controller details |
| `/redfish/v1/Systems/{id}/Storage/{id}/Drives/{id}` | GET | Drive details |
| `/redfish/v1/Systems/{id}/Storage/{id}/Volumes` | POST | Create RAID volume (Dell) |

---

## NVIDIA OEM Extensions

**Code**: Various methods in libredfish `nvidia_dpu.rs`, `nvidia_gh200.rs`, `nvidia_gb200.rs`, `nvidia_gbswitch.rs`

| Endpoint | Method | Vendor | Purpose |
|----------|--------|--------|---------|
| `Systems/{id}/Oem/Nvidia` | GET | NVIDIA DPU | Base MAC, rshim status, NIC mode |
| `Systems/{id}/Oem/Nvidia/Actions/HostRshim.Set` | POST | NVIDIA DPU | Set rshim (BF3) |
| `Systems/{id}/Oem/Nvidia/Actions/Mode.Set` | POST | NVIDIA DPU | Set NIC/DPU mode |
| `Managers/Bluefield_BMC/Oem/Nvidia` | PATCH | NVIDIA DPU | Enable rshim |
| `Chassis/BMC_0/Actions/Oem/NvidiaChassis.AuxPowerReset` | POST | NVIDIA GBx00 | AC power cycle |
| `Chassis/HGX_Chassis_0` | GET | NVIDIA GBx00 | HGX chassis info |
| `Systems/HGX_Baseboard_0/Processors` | GET | NVIDIA GBx00 | GPU enumeration (DPS) |
| `Systems/HGX_Baseboard_0/Processors/{id}/Oem/Nvidia/WorkloadPowerProfile` | GET/POST | NVIDIA GBx00 | WPPS config (DPS) |

---

## CI/CD Pipeline Endpoints

These endpoints are used by the CI/CD tooling (`cicd/redfish_cli.py`, `cicd/install_wrapper.py`) and are **not** part of core NICo.

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `{System}/VirtualMedia` or `{Manager}/VirtualMedia` | GET | Virtual media devices |
| `{VirtualMedia}/Actions/VirtualMedia.InsertMedia` | POST | Mount ISO image |
| `{VirtualMedia}/Actions/VirtualMedia.EjectMedia` | POST | Eject media |
| `Systems/{id}` | PATCH | Boot source override (CD once) |
| `{Manager}/HostInterfaces/{id}` | PATCH | Enable/disable OS-to-BMC NIC |
| `SessionService/Sessions` | POST | Create auth session |
