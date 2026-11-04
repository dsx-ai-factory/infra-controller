# On-Demand Rack Maintenance

On-demand maintenance allows an operator to trigger a maintenance cycle on a rack that is in the **Ready** or **Error** state. It supports both **full-rack** and **partial-rack** scoping — the caller can optionally specify which machines, switches, or power shelves to maintain, and which maintenance **activities** to perform.

## Scope: Full Rack vs Partial Rack

The maintenance request carries an optional **`MaintenanceScope`** that specifies which devices to include:

| Scenario | `machine_ids` | `switch_ids` | `power_shelf_ids` | Effect |
|----------|--------------|--------------|-------------------|--------|
| Full rack | *(empty)* | *(empty)* | *(empty)* | All machines, switches, and power shelves in the rack are maintained. |
| Machines only | `[id1, id2]` | *(empty)* | *(empty)* | Only the specified machines are reprovisioned and firmware-upgraded. Switches and power shelves are skipped. |
| Mixed | `[id1]` | `[sid1]` | *(empty)* | Only the listed machines and switches are maintained; power shelves are skipped. |

When no device IDs are specified (all three lists empty), the scope is treated as **full rack** — identical to the existing `reprovision_requested` behavior.

## Activities

The request also carries an optional list of **maintenance activities** to perform. When the list is empty, all non-power activities are selected. On-demand compute-host reboots require an explicit `PowerSequence` activity. Available activities:

| Activity | Proto `oneof` variant | Per-activity config | Description |
|----------|----------------------|---------------------|-------------|
| Firmware Upgrade | `FirmwareUpgradeActivity` | `firmware_version`: SOT JSON (required for explicit selection). `components`: empty selects all. `access_token`: optional. `force_update`: defaults to false. | Reprovisioning and firmware upgrade via RMS. |
| NVOS Update | `NvosUpdateActivity` | `config_json`: SOT JSON passed to RMS `ApplySwitchSystemImage` (required for explicit selection). `access_token`: optional. | NVOS image update for scoped switches through RMS. |
| Power Sequence | `PowerSequenceActivity` | None | Gracefully reboot all rack compute hosts before NMX-C configuration; device scope does not narrow this batch. |
| Configure NMX Cluster | `ConfigureNmxClusterActivity` | None | NMX cluster configuration. |

Each activity is represented as a `MaintenanceActivityConfig` message with a `oneof activity` field. The `oneof` discriminant identifies the activity type, and each variant carries only the configuration fields relevant to that activity.

Selected activities execute in firmware upgrade, NVOS update, power sequence, then NMX-C configuration order, regardless of request ordering. An on-demand NVOS update alone does not reboot compute hosts. See the [rack maintenance flow](rackstatemachine.md#maintenance-r_maintenance) for ingestion, reboot completion, and resume behavior.

### Firmware Configuration Source

The update configuration depends on how maintenance was requested:

| Trigger | Configuration source |
|---------|----------------------|
| Ingestion or ordinary reprovision | Rack-profile `firmware_object` |
| On-demand, activities omitted or empty | Rack-profile `firmware_object` |
| On-demand, explicit firmware upgrade | SOT JSON in `FirmwareUpgradeActivity.firmware_version` |
| On-demand, explicit NVOS update | SOT JSON in `NvosUpdateActivity.config_json` |

Explicit update activities require nonempty, valid JSON; the API rejects missing or invalid JSON. Despite its name, `firmware_version` carries SOT JSON, not a firmware record ID. Omitted or empty access tokens are passed to RMS as `NOAUTH`.

Without a configured rack-profile firmware object, implicit update phases are skipped. NVOS update instead enters `Error` if a scoped switch is already waiting for that update. See the [rack maintenance flow](rackstatemachine.md#maintenance-r_maintenance) for details.

## Flow

```text
┌────────┐       ┌──────────────┐       ┌──────────┐       ┌─────────────────┐
│ Caller │──────▶│ gRPC Endpoint│──────▶│ Postgres │       │ Rack State      │
│ (CLI)  │       │ OnDemandRack │       │          │       │ Handler         │
│        │       │ Maintenance  │       │          │       │                 │
└────────┘       └──────┬───────┘       └────┬─────┘       └────────┬────────┘
                        │                    │                      │
                        │ 1. Load rack       │                      │
                        │ verify Ready/Error │                      │
                        │ 2. Set config.     │                      │
                        │    maintenance_    │                      │
                        │    requested=scope │                      │
                        │──────────────────▶ │                      │
                        │                    │                      │
                        │ 3. Return OK       │                      │
                        │◀───────────────────│                      │
                        │                    │                      │
                        │                    │  4. Poll rack       │
                        │                    │◀─────────────────────│
                        │                    │                      │
                        │                    │  5. Detect           │
                        │                    │     maintenance_     │
                        │                    │     requested        │
                        │                    │                      │
                        │                    │  6. Transition to    │
                        │                    │     Maintenance      │
                        │                    │ (first selected     │
                        │                    │  activity)          │
                        │                    │◀─────────────────────│
```

1. The caller invokes the `OnDemandRackMaintenance` gRPC method with a `rack_id` and optional device-ID lists.
2. The handler validates that the rack is in `Ready` or `Error` state and no maintenance is already pending.
3. It writes a `MaintenanceScope` to `RackConfig.maintenance_requested` and persists the config.
4. On the next state-handler tick, `handle_ready` or `handle_error` detects `maintenance_requested` and enters the first selected maintenance activity.
5. Update activities use the configuration source described above and filter device reprovisioning and firmware-upgrade operations to the scope. An explicitly selected power sequence gracefully reboots all rack compute hosts, regardless of device scope.
6. After maintenance completes, the rack flows through `Validating` back to `Ready` as usual.

## gRPC API

**Service method** (in `NICo` service):

```protobuf
rpc OnDemandRackMaintenance(RackMaintenanceOnDemandRequest) returns (RackMaintenanceOnDemandResponse);
```

**Messages**:

```protobuf
message FirmwareUpgradeActivity {
  string firmware_version = 1;  // SOT JSON, required for explicit selection
  repeated string components = 2;  // empty = all components
  optional string access_token = 3;  // omitted/empty = NOAUTH
  bool force_update = 4;
}
message NvosUpdateActivity {
  reserved 1;
  reserved "firmware_object_id";
  string config_json = 2;  // SOT JSON, required for explicit selection
  optional string access_token = 3;  // omitted/empty = NOAUTH
}
message ConfigureNmxClusterActivity {}
message PowerSequenceActivity {}  // explicit graceful compute-host reboot

message MaintenanceActivityConfig {
  oneof activity {
    FirmwareUpgradeActivity firmware_upgrade = 1;
    ConfigureNmxClusterActivity configure_nmx_cluster = 2;
    PowerSequenceActivity power_sequence = 3;
    NvosUpdateActivity nvos_update = 4;
  }
}

message RackMaintenanceScope {
  repeated string machine_ids = 1;
  repeated string switch_ids = 2;
  repeated string power_shelf_ids = 3;
  repeated MaintenanceActivityConfig activities = 4;  // empty = all except power sequence
}

message RackMaintenanceOnDemandRequest {
  common.RackId rack_id = 1;
  RackMaintenanceScope scope = 2;  // unset/empty = full rack, non-power activities
}

message RackMaintenanceOnDemandResponse {}
```

## Component Manager Integration

The `UpdateComponentFirmware` gRPC endpoint provides a higher-level interface for firmware updates. For **compute trays** and **switches**, it delegates to the rack state machine by internally calling `on_demand_rack_maintenance`, rather than managing firmware directly.

### How it works

When `update_component_firmware` receives a request targeting compute trays or switches:

1. **Resolve the rack** — looks up the first device (machine or switch) in the database to find its `rack_id`.
2. **Build a maintenance request** — constructs a `RackMaintenanceOnDemandRequest` with:
   - The resolved `rack_id`
   - The machine IDs and/or switch IDs from the request
   - A `FirmwareUpgrade` activity carrying the `target_version` as `firmware_version`
3. **Delegate** — calls `on_demand_rack_maintenance`, which writes the `MaintenanceScope` to the rack config and lets the rack state machine handle the actual firmware upgrade.
4. **Return success** — once the maintenance is scheduled, returns a success `ComponentResult` for each device.

Power shelves continue to use the component manager backend directly (they do not go through the rack state machine).

```text
┌──────────────┐      ┌──────────────────────┐      ┌──────────────────────┐
│ Caller       │─────▶│ UpdateComponent       │─────▶│ OnDemandRack         │
│              │      │ Firmware (gRPC)       │      │ Maintenance (gRPC)   │
└──────────────┘      └──────────┬───────────┘      └──────────┬───────────┘
                                  │                             │
                      ┌───────────▼──────────┐      ┌──────────▼───────────┐
                      │ Resolve rack_id from │      │ Write maintenance_   │
                      │ machine/switch DB    │      │ requested to config  │
                      └──────────────────────┘      └──────────┬───────────┘
                                                               │
                                                    ┌──────────▼───────────┐
                                                    │ Rack State Machine   │
                                                    │ Ready → Maintenance  │
                                                    │ (FirmwareUpgrade/    │
                                                    │  Start)              │
                                                    └──────────────────────┘
```

| Target type | Behavior |
|-------------|----------|
| `ComputeTrays` | Resolves `rack_id` from first machine, delegates to `on_demand_rack_maintenance` with `machine_ids` and `firmware_version` |
| `Switches` | Resolves `rack_id` from first switch, delegates to `on_demand_rack_maintenance` with `switch_ids` and `firmware_version` |
| `PowerShelves` | Handled directly by the component manager power shelf backend (no state machine interaction) |

### Example: compute tray firmware upgrade

1. The caller sends `UpdateComponentFirmware` with `ComputeTrays { machine_ids: [machine-001, machine-002] }` and the SOT JSON object encoded in `target_version`.
2. Machine `machine-001` is looked up to discover `rack_id = rack-42`.
3. `OnDemandRackMaintenance` is called with `rack_id = rack-42`, `machine_ids = [machine-001, machine-002]`, and a `FirmwareUpgradeActivity` carrying that same JSON in `firmware_version`.
4. The rack state machine runs the firmware upgrade via RMS for the specified machines, using the supplied JSON rather than looking up a firmware record ID.

## Preconditions

The gRPC handler rejects the request with an error if:

- The rack is **not in `Ready` or `Error` state** — maintenance can only be triggered from these two states.
- A maintenance request is **already pending** (`maintenance_requested` is already set).
- Any provided device ID is **malformed** (cannot be parsed).

## RBAC

The `OnDemandRackMaintenance` permission is granted to the `NicoAdminCLI` role.

## Failure Handling

If the maintenance state machine transitions to `Error` (for example, a
firmware upgrade fails, the requested rack firmware cannot be found, or RMS is
unreachable), the handler clears `maintenance_requested` while persisting the
`Error` transition.

This is important because `handle_error` re-enters `Maintenance` whenever
`maintenance_requested` is set; without clearing it, the rack would loop
between `Maintenance` and `Error` on the same failing request. The user must
issue a new `OnDemandRackMaintenance` call to retry.

### Restarting a compute stuck in `FailedFirmwareUpgrade`

A compute tray whose host firmware upgrade fails lands in
`M_HostReprovision::FailedFirmwareUpgrade`. From there it normally retries
automatically (bounded by `MAX_FIRMWARE_UPGRADE_RETRIES` and
`host_firmware_upgrade_retry_interval`).

When an on-demand maintenance call (or, equivalently, the rack maintenance
flow) issues a fresh `trigger_host_reprovisioning_request` against such a
machine, `host_reprovisioning_requested` is overwritten with
`started_at = None`. The `FailedFirmwareUpgrade` arm in
`HostUpgradeState::handle_host_reprovision` detects this fresh request
(`started_at.is_none()`) and:

- For rack-initiated requests (initiator prefixed `rack-`), transitions to
  `M_HostReprovision::WaitingForRackFirmwareUpgrade` so the rack-level RMS
  flow can drive the upgrade.
- Otherwise transitions to `M_HostReprovision::CheckingFirmwareV2` with
  `retry_count` reset to `0`, mirroring the way `ManagedHostState::Ready`
  kicks off a Host Reprovision (including the `host-fw-update` health-report
  alert merge).

This means an on-demand maintenance call always converges a stuck compute back
toward `M_Ready` without waiting for the auto-retry interval, which is what
allows the rack to progress out of `R_Maintenance` into `R_Validation`.

## Ready State Priority

When the rack is in `Ready`, three config flags are checked in order. The first match wins:

1. **`topology_changed`** → transition to `Discovering`
2. **`reprovision_requested`** → transition to `Maintenance(FirmwareUpgrade/Start)` *(clears any pending `maintenance_requested`)*
3. **`maintenance_requested`** → transition to `Maintenance(FirmwareUpgrade/Start)` with device scope

## Implementation

| Component | Location |
|-----------|----------|
| Scope model (`MaintenanceScope`) | `crates/api-model/src/rack.rs` |
| Config field (`maintenance_requested`) | `RackConfig` in the same file |
| gRPC handler | `on_demand_rack_maintenance` in `crates/api/src/handlers/rack.rs` |
| API wiring | `crates/api/src/api.rs` |
| RBAC rule | `crates/api/src/auth/internal_rbac_rules.rs` |
| Ready state check | `handle_ready` in `crates/api/src/state_controller/rack/ready.rs` |
| Firmware resolution & upgrade start | `start_firmware_upgrade` in `crates/api/src/state_controller/rack/maintenance.rs` |
| Maintenance state dispatch | `handle_maintenance` in `crates/api/src/state_controller/rack/maintenance.rs` |
| Component manager firmware entry point | `update_component_firmware` in `crates/api/src/handlers/component_manager.rs` |
| Protobuf definitions | `crates/rpc/proto/nico.proto` |
