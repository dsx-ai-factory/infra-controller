# PowerShelf Manager gRPC API Reference

**Version:** `v1`  
**Proto:** `internal/proto/v1/powershelf-manager.proto`

---

## Overview

PowerShelf Manager (PSM) provides gRPC APIs for managing power shelves via their Power Management Controllers (PMCs). The service supports:

- **Registration** — Onboard PMCs with credentials
- **Inventory** — Query powershelf hardware, firmware, and sensor data
- **Firmware Management** — List available upgrades, trigger updates, monitor status
- **Power Control** — Chassis power on/off via Redfish

All batch endpoints accept multiple targets and return per-target responses with individual status codes, enabling partial success handling.

---

## Connection

| Property     | Value                                            |
|--------------|--------------------------------------------------|
| Transport    | gRPC over HTTP/2                                 |
| TLS          | Auto-detected via `CERTDIR` environment variable |
| Default Port | 50051                                            |

### Plaintext (development)

When no certificates are present, the server runs in plaintext mode:

```bash
./psm serve --port 50051

grpcurl -plaintext localhost:50051 list v1.PowershelfManager
```

### mTLS (production)

The server automatically enables mutual TLS when certificates are found. Set `CERTDIR` to the certificate directory (defaults to `/var/run/secrets/spiffe.io` for Kubernetes SPIFFE integration).

**Required files in `CERTDIR`:**

| File      | Description                        |
|-----------|------------------------------------|
| `ca.crt`  | CA certificate for client verification |
| `tls.crt` | Server certificate                 |
| `tls.key` | Server private key                 |

```bash
# Local development with mTLS
export CERTDIR=/path/to/certs
./psm serve --port 50051

# Client must provide certificate
grpcurl -cacert $CERTDIR/ca.crt \
        -cert $CERTDIR/tls.crt \
        -key $CERTDIR/tls.key \
        localhost:50051 list v1.PowershelfManager
```

**Kubernetes deployment:**

Certificates are typically mounted via SPIFFE/SPIRE at `/var/run/secrets/spiffe.io`. The server auto-detects this path when `CERTDIR` is unset.

---

## Observability

### Structured Logging

The service uses structured JSON logging via [logrus](https://github.com/sirupsen/logrus). All gRPC requests and responses are automatically logged via middleware ([go-grpc-middleware](https://github.com/grpc-ecosystem/go-grpc-middleware)).

**Request log fields:**

| Field          | Description                              |
|----------------|------------------------------------------|
| `grpc.method`  | Full RPC method path                     |
| `grpc.request` | Request payload as JSON                  |

**Response log fields:**

| Field           | Description                              |
|-----------------|------------------------------------------|
| `grpc.method`   | Full RPC method path                     |
| `grpc.code`     | gRPC status code (`OK`, `INVALID_ARGUMENT`, etc.) |
| `grpc.duration` | Request processing time                  |
| `grpc.response` | Response payload as JSON (on success)    |
| `grpc.error`    | Error message (on failure)               |

**Example output:**

```json
{"level":"info","msg":"gRPC request","grpc.method":"/v1.PowershelfManager/ListAvailableFirmware","grpc.request":{"pmcMacs":["24:5B:F0:80:BB:A5"]}}
{"level":"info","msg":"gRPC response","grpc.method":"/v1.PowershelfManager/ListAvailableFirmware","grpc.code":"OK","grpc.duration":"10.5ms","grpc.response":{"upgrades":[...]}}
```

**Note:** Request/response payloads may contain sensitive data. Consider log redaction for production environments.

---

## Status Codes

All responses include a `StatusCode` field indicating the result of each operation:

| Code               | Value | Description                                                      |
|--------------------|-------|------------------------------------------------------------------|
| `SUCCESS`          | 0     | Operation completed successfully                                 |
| `INVALID_ARGUMENT` | 1     | Client error: malformed MAC address, unsupported component, etc. |
| `INTERNAL_ERROR`   | 2     | Server error: database failure, Redfish timeout, etc.            |

When `status != SUCCESS`, the `error` field contains a human-readable message.

---

## Enums

### PMCVendor

Identifies the PMC hardware vendor. Determines firmware compatibility and Redfish dialect.

| Value              | Code | Notes                            |
|--------------------|------|----------------------------------|
| `PMC_TYPE_UNKNOWN` | 0    | Unsupported or unrecognized vendor |
| `PMC_TYPE_LITEON`  | 1    | Lite-On PMC                      |

### Component

Firmware-upgradable component within a powershelf.

| Value | Code | Notes                              |
|-------|------|------------------------------------|
| `PMC` | 0    | Power Management Controller        |
| `PSU` | 1    | Power Supply Unit (not yet supported) |

### FirmwareUpdateState

State machine for firmware update operations.

| Value                            | Code | Description                          |
|----------------------------------|------|--------------------------------------|
| `FIRMWARE_UPDATE_STATE_UNKNOWN`  | 0    | State could not be determined        |
| `FIRMWARE_UPDATE_STATE_QUEUED`   | 1    | Update scheduled, awaiting execution |
| `FIRMWARE_UPDATE_STATE_VERIFYING`| 2    | Update in progress, verifying firmware |
| `FIRMWARE_UPDATE_STATE_COMPLETED`| 3    | Update finished successfully         |
| `FIRMWARE_UPDATE_STATE_FAILED`   | 4    | Update failed; check error message   |

---

## RPCs

### RegisterPowershelves

Registers one or more powershelves by their PMC identifiers and persists credentials for subsequent Redfish operations.

```protobuf
rpc RegisterPowershelves(RegisterPowershelvesRequest) returns (RegisterPowershelvesResponse)
```

#### Request

```protobuf
message RegisterPowershelvesRequest {
    repeated RegisterPowershelfRequest registration_requests = 1;
}

message RegisterPowershelfRequest {
    string pmc_mac_address = 1;   // Required. IEEE 802 MAC (e.g., "00:11:22:33:44:55")
    string pmc_ip_address = 2;    // Required. IPv4 address for Redfish endpoint
    PMCVendor pmc_vendor = 3;     // Required. Hardware vendor
    Credentials pmc_credentials = 4; // Required. Redfish credentials
}

message Credentials {
    string username = 1;
    string password = 2;
}
```

#### Response

```protobuf
message RegisterPowershelvesResponse {
    repeated RegisterPowershelfResponse responses = 1;
}

message RegisterPowershelfResponse {
    string pmc_mac_address = 1;      // Echoed from request
    bool is_new = 2;                 // true if newly created, false if updated
    google.protobuf.Timestamp created = 3;
    StatusCode status = 4;
    string error = 5;
}
```

#### Behavior

- Idempotent: re-registering an existing MAC updates credentials
- Credentials are stored encrypted; see credential manager configuration
- MAC address serves as the primary key

#### Example

```bash
grpcurl -plaintext -d '{
  "registration_requests": [{
    "pmc_mac_address": "00:11:22:33:44:55",
    "pmc_ip_address": "10.0.1.100",
    "pmc_vendor": "PMC_TYPE_LITEON",
    "pmc_credentials": {"username": "admin", "password": "secret"}
  }]
}' localhost:50051 v1.PowershelfManager/RegisterPowershelves
```

---

### GetPowershelves

Retrieves full inventory for the specified powershelves, including PMC metadata, chassis info, and PSU sensor readings.

```protobuf
rpc GetPowershelves(PowershelfRequest) returns (GetPowershelvesResponse)
```

#### Request

```protobuf
message PowershelfRequest {
    repeated string pmc_macs = 1;  // Optional. Empty = return all registered
}
```

#### Response

```protobuf
message GetPowershelvesResponse {
    repeated PowerShelf powershelves = 1;
}

message PowerShelf {
    PowerManagementController pmc = 1;
    Chassis chassis = 2;
    repeated PowerSupplyUnit psus = 3;
}
```

#### PowerManagementController

| Field              | Type      | Description               |
|--------------------|-----------|---------------------------|
| `mac_address`      | string    | IEEE 802 MAC address      |
| `ip_address`       | string    | Redfish endpoint IP       |
| `vendor`           | PMCVendor | Hardware vendor           |
| `serial_number`    | string    | From Redfish Manager      |
| `model`            | string    | Hardware model            |
| `manufacturer`     | string    | OEM name                  |
| `part_number`      | string    | OEM part number           |
| `firmware_version` | string    | Current firmware version  |
| `hardware_version` | string    | Hardware revision         |

#### Chassis

| Field           | Type   | Description   |
|-----------------|--------|---------------|
| `serial_number` | string | Chassis serial |
| `model`         | string | Chassis model  |
| `manufacturer`  | string | Chassis OEM    |

#### PowerSupplyUnit

| Field              | Type     | Description                    |
|--------------------|----------|--------------------------------|
| `id`               | string   | PSU identifier (e.g., "PSU1")  |
| `name`             | string   | Human-readable name            |
| `serial_number`    | string   | PSU serial                     |
| `model`            | string   | PSU model                      |
| `manufacturer`     | string   | PSU OEM                        |
| `firmware_version` | string   | PSU firmware                   |
| `hardware_version` | string   | PSU hardware rev               |
| `capacity_watts`   | string   | Rated capacity                 |
| `power_state`      | bool     | true = powered on              |
| `sensors`          | Sensor[] | Telemetry readings             |

#### Sensor

| Field             | Type             | Description                  |
|-------------------|------------------|------------------------------|
| `id`              | string           | Sensor identifier            |
| `name`            | string           | Human-readable name          |
| `reading`         | float            | Current value                |
| `reading_units`   | string           | Unit (e.g., "Cel", "V", "A") |
| `reading_type`    | string           | Sensor type                  |
| `reading_range_min` | float          | Valid range minimum          |
| `reading_range_max` | double         | Valid range maximum          |
| `thresholds`      | SensorThresholds | Alarm thresholds             |

#### Example

```bash
# Get all powershelves
grpcurl -plaintext -d '{}' localhost:50051 v1.PowershelfManager/GetPowershelves

# Get specific powershelves
grpcurl -plaintext -d '{
  "pmc_macs": ["00:11:22:33:44:55", "00:11:22:33:44:56"]
}' localhost:50051 v1.PowershelfManager/GetPowershelves
```

---

### ListAvailableFirmware

Returns available firmware upgrade paths for each specified powershelf, based on current version and vendor-specific compatibility rules.

```protobuf
rpc ListAvailableFirmware(PowershelfRequest) returns (ListAvailableFirmwareResponse)
```

#### Response

```protobuf
message ListAvailableFirmwareResponse {
    repeated AvailableFirmware upgrades = 1;
}

message AvailableFirmware {
    string pmc_mac_address = 1;
    repeated ComponentFirmwareUpgrades upgrades = 2;
}

message ComponentFirmwareUpgrades {
    Component component = 1;
    repeated FirmwareVersion upgrades = 2;  // Valid target versions
}

message FirmwareVersion {
    string version = 1;  // Semantic version (e.g., "1.2.3")
}
```

#### Behavior

- Returns only versions the PMC can upgrade to from its current version
- Empty `upgrades` array indicates no upgrades available
- Version compatibility is vendor-specific

#### Example

```bash
grpcurl -plaintext -d '{
  "pmc_macs": ["00:11:22:33:44:55"]
}' localhost:50051 v1.PowershelfManager/ListAvailableFirmware
```

---

### UpdateFirmware

Initiates firmware updates for one or more powershelves. Updates are queued and executed asynchronously.

```protobuf
rpc UpdateFirmware(UpdateFirmwareRequest) returns (UpdateFirmwareResponse)
```

#### Request

```protobuf
message UpdateFirmwareRequest {
    repeated UpdatePowershelfFirmwareRequest upgrades = 1;
}

message UpdatePowershelfFirmwareRequest {
    string pmc_mac_address = 1;
    repeated UpdateComponentFirmwareRequest components = 2;
}

message UpdateComponentFirmwareRequest {
    Component component = 1;      // PMC or PSU
    FirmwareVersion upgradeTo = 2; // Target version
}
```

#### Response

```protobuf
message UpdateFirmwareResponse {
    repeated UpdatePowershelfFirmwareResponse responses = 1;
}

message UpdatePowershelfFirmwareResponse {
    string pmc_mac_address = 1;
    repeated UpdateComponentFirmwareResponse components = 2;
}

message UpdateComponentFirmwareResponse {
    Component component = 1;
    StatusCode status = 2;
    string error = 3;
}
```

#### Behavior

- Returns immediately after queueing; use `GetFirmwareUpdateStatus` to poll
- Only `PMC` component is currently supported; `PSU` returns `INVALID_ARGUMENT`
- Target version must be in the `ListAvailableFirmware` response
- Concurrent updates to the same PMC are rejected

#### Example

```bash
grpcurl -plaintext -d '{
  "upgrades": [{
    "pmc_mac_address": "00:11:22:33:44:55",
    "components": [{
      "component": "PMC",
      "upgradeTo": {"version": "2.0.0"}
    }]
  }]
}' localhost:50051 v1.PowershelfManager/UpdateFirmware
```

---

### GetFirmwareUpdateStatus

Queries the current state of firmware update operations for specific PMC/component pairs.

```protobuf
rpc GetFirmwareUpdateStatus(GetFirmwareUpdateStatusRequest) returns (GetFirmwareUpdateStatusResponse)
```

#### Request

```protobuf
message GetFirmwareUpdateStatusRequest {
    repeated FirmwareUpdateQuery queries = 1;
}

message FirmwareUpdateQuery {
    string pmc_mac_address = 1;  // Required. Target PMC
    Component component = 2;      // Required. PMC or PSU
}
```

#### Response

```protobuf
message GetFirmwareUpdateStatusResponse {
    repeated FirmwareUpdateStatus statuses = 1;
}

message FirmwareUpdateStatus {
    string pmc_mac_address = 1;
    Component component = 2;
    FirmwareUpdateState state = 3;  // Current state in the update lifecycle
    StatusCode status = 4;          // Request status (SUCCESS if record found)
    string error = 5;               // Error message if status != SUCCESS
}
```

#### Behavior

- Returns the most recent update record for each PMC/component pair
- `status = INTERNAL_ERROR` with appropriate `error` if no record exists
- State transitions: `QUEUED` → `VERIFYING` → `COMPLETED` | `FAILED`

#### Example

```bash
grpcurl -plaintext -d '{
  "queries": [
    {"pmc_mac_address": "00:11:22:33:44:55", "component": "PMC"},
    {"pmc_mac_address": "00:11:22:33:44:56", "component": "PMC"}
  ]
}' localhost:50051 v1.PowershelfManager/GetFirmwareUpdateStatus
```

#### Polling Pattern

```go
func waitForCompletion(ctx context.Context, client pb.PowershelfManagerClient, mac string, component pb.PowershelfComponent, timeout time.Duration) error {
    ctx, cancel := context.WithTimeout(ctx, timeout)
    defer cancel()

    ticker := time.NewTicker(5 * time.Second)
    defer ticker.Stop()

    for {
        select {
        case <-ctx.Done():
            return fmt.Errorf("firmware update timed out")
        case <-ticker.C:
            resp, err := client.GetFirmwareUpdateStatus(ctx, &pb.GetFirmwareUpdateStatusRequest{
                Queries: []*pb.FirmwareUpdateQuery{
                    {PmcMacAddress: mac, Component: component},
                },
            })
            if err != nil {
                return err
            }

            state := resp.Statuses[0].State
            switch state {
            case pb.FirmwareUpdateState_FIRMWARE_UPDATE_STATE_COMPLETED:
                return nil
            case pb.FirmwareUpdateState_FIRMWARE_UPDATE_STATE_FAILED:
                return fmt.Errorf("update failed: %s", resp.Statuses[0].Error)
            }
        }
    }
}
```

---

### SetDryRun

Configures the firmware manager's dry-run mode. When enabled, firmware operations validate artifacts and simulate updates without uploading to the PMC.

```protobuf
rpc SetDryRun(SetDryRunRequest) returns (google.protobuf.Empty)
```

#### Request

```protobuf
message SetDryRunRequest {
    bool dry_run = 1;  // true = enable dry-run, false = disable
}
```

#### Behavior

- Global setting affecting all subsequent `UpdateFirmware` calls
- Persists until changed or service restart
- Default: disabled (real updates)

#### Example

```bash
# Enable dry-run mode
grpcurl -plaintext -d '{"dry_run": true}' localhost:50051 v1.PowershelfManager/SetDryRun

# Disable dry-run mode
grpcurl -plaintext -d '{"dry_run": false}' localhost:50051 v1.PowershelfManager/SetDryRun
```

---

### PowerOff

Issues a Redfish chassis power-off action for each specified powershelf. Supports both registered devices (by MAC) and unregistered devices (by inline connection details via `PowerTarget`).

```protobuf
rpc PowerOff(PowerRequest) returns (PowerControlResponse)
```

#### Request

```protobuf
message PowerRequest {
    repeated string pmc_macs = 1;       // Registered devices by MAC
    repeated PowerTarget targets = 2;   // Unregistered devices with inline credentials
}

message PowerTarget {
    string pmc_ip = 1;
    Credentials pmc_credentials = 2;
    PMCVendor pmc_vendor = 3;
}
```

#### Response

```protobuf
message PowerControlResponse {
    repeated PowershelfResponse responses = 1;
}

message PowershelfResponse {
    string pmc_mac_address = 1;  // Set for registered devices; empty for direct targets
    StatusCode status = 2;
    string error = 3;
    string pmc_ip = 4;           // Set for direct PowerTarget responses; empty for registered devices
}
```

#### Behavior

- Executes `Chassis.Reset` with `ResetType: ForceOff`
- Idempotent: powering off an already-off chassis succeeds
- Returns per-target status; partial failures are possible
- For registered devices, `pmc_mac_address` identifies the device in the response
- For direct `PowerTarget` requests, `pmc_ip` identifies the device and `pmc_mac_address` is empty

#### Example

```bash
# Registered device by MAC
grpcurl -plaintext -d '{
  "pmc_macs": ["00:11:22:33:44:55"]
}' localhost:50051 v1.PowershelfManager/PowerOff

# Unregistered device via PowerTarget
grpcurl -plaintext -d '{
  "targets": [{
    "pmc_ip": "10.0.1.100",
    "pmc_credentials": {"username": "admin", "password": "secret"},
    "pmc_vendor": "PMC_TYPE_LITEON"
  }]
}' localhost:50051 v1.PowershelfManager/PowerOff
```

---

### PowerOn

Issues a Redfish chassis power-on action for each specified powershelf. Supports both registered devices (by MAC) and unregistered devices (by inline connection details via `PowerTarget`).

```protobuf
rpc PowerOn(PowerRequest) returns (PowerControlResponse)
```

#### Behavior

- Executes `Chassis.Reset` with `ResetType: On`
- Idempotent: powering on an already-on chassis succeeds
- Returns per-target status; partial failures are possible
- Response format is the same as `PowerOff`: `pmc_mac_address` for registered devices, `pmc_ip` for direct targets

#### Example

```bash
# Registered device by MAC
grpcurl -plaintext -d '{
  "pmc_macs": ["00:11:22:33:44:55"]
}' localhost:50051 v1.PowershelfManager/PowerOn

# Unregistered device via PowerTarget
grpcurl -plaintext -d '{
  "targets": [{
    "pmc_ip": "10.0.1.100",
    "pmc_credentials": {"username": "admin", "password": "secret"},
    "pmc_vendor": "PMC_TYPE_LITEON"
  }]
}' localhost:50051 v1.PowershelfManager/PowerOn
```

---

## Error Handling

### Partial Failures

Batch operations return individual status codes per target. Always iterate through responses:

```go
resp, err := client.RegisterPowershelves(ctx, req)
if err != nil {
    // Transport/connection error
    return err
}

for _, r := range resp.Responses {
    if r.Status != pb.StatusCode_SUCCESS {
        log.Errorf("Failed to register %s: %s", r.PmcMacAddress, r.Error)
    }
}
```

### Common Error Scenarios

| Scenario              | StatusCode         | Error Message Pattern                         |
|-----------------------|--------------------|-----------------------------------------------|
| Invalid MAC format    | `INVALID_ARGUMENT` | `invalid MAC address: ...`                    |
| Unsupported component | `INVALID_ARGUMENT` | `PSM does not support upgrading PSU component` |
| PMC not registered    | `INTERNAL_ERROR`   | `PMC not found`                               |
| Redfish unreachable   | `INTERNAL_ERROR`   | `connection refused` / `timeout`              |
| Database error        | `INTERNAL_ERROR`   | `failed to query: ...`                        |

---

## Versioning

The API follows semantic versioning. Breaking changes increment the package version (`v1` → `v2`). The current version is **v1**.

Non-breaking changes (new fields, new RPCs) are added to the existing package.
