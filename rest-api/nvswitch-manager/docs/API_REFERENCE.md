# NV-Switch Manager gRPC API Reference

**Version:** `v1`  
**Proto:** `internal/proto/v1/nvswitch-manager.proto`

---

## Overview

NV-Switch Manager (NSM) provides gRPC APIs for managing NVIDIA DGX GB200 NVLink Switch Trays. The service supports:

- **Registration** — Onboard NV-Switch trays with BMC and NVOS credentials
- **Inventory** — Query switch hardware, firmware, and subsystem data
- **Firmware Management** — List bundles, queue updates, monitor status, cancel operations
- **Power Control** — Chassis power cycle via Redfish

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
./nvswitch-manager serve --port 50051

grpcurl -plaintext localhost:50051 list v1.NVSwitchManager
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
./nvswitch-manager serve --port 50051

# Client must provide certificate
grpcurl -cacert $CERTDIR/ca.crt \
        -cert $CERTDIR/tls.crt \
        -key $CERTDIR/tls.key \
        localhost:50051 list v1.NVSwitchManager
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
{"level":"info","msg":"gRPC request","grpc.method":"/v1.NVSwitchManager/ListBundles","grpc.request":{}}
{"level":"info","msg":"gRPC response","grpc.method":"/v1.NVSwitchManager/ListBundles","grpc.code":"OK","grpc.duration":"10.5ms","grpc.response":{"bundles":[...]}}
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

### Vendor

Identifies the NV-Switch tray hardware vendor.

| Value              | Code | Notes                            |
|--------------------|------|----------------------------------|
| `VENDOR_UNKNOWN`   | 0    | Unsupported or unrecognized vendor |
| `VENDOR_NVIDIA`    | 1    | NVIDIA                           |

### NVSwitchComponent

Firmware-upgradable component within an NV-Switch tray. Update sequence: BMC -> CPLD -> BIOS -> NVOS.

| Value                          | Code | Notes                              |
|--------------------------------|------|------------------------------------|
| `NVSWITCH_COMPONENT_UNKNOWN`   | 0    | Unknown component                  |
| `NVSWITCH_COMPONENT_BMC`       | 1    | BMC firmware via Redfish           |
| `NVSWITCH_COMPONENT_CPLD`      | 2    | CPLD via NVOS SSH                  |
| `NVSWITCH_COMPONENT_BIOS`      | 3    | BIOS firmware via Redfish          |
| `NVSWITCH_COMPONENT_NVOS`      | 4    | NVOS system image via SSH          |

### UpdateStrategy

Specifies how firmware is delivered and installed.

| Value                          | Code | Notes                              |
|--------------------------------|------|------------------------------------|
| `UPDATE_STRATEGY_UNKNOWN`      | 0    | Unknown strategy                   |
| `UPDATE_STRATEGY_SCRIPT`       | 1    | External shell scripts             |
| `UPDATE_STRATEGY_SSH`          | 2    | Direct SSH commands                |
| `UPDATE_STRATEGY_REDFISH`      | 3    | Redfish API                        |

### UpdateState

State machine for firmware update operations.

| Value                          | Code | Description                          |
|--------------------------------|------|--------------------------------------|
| `UPDATE_STATE_UNKNOWN`         | 0    | State could not be determined        |
| `UPDATE_STATE_QUEUED`          | 1    | Update scheduled, awaiting execution |
| `UPDATE_STATE_POWER_CYCLE`     | 2    | Power cycling via BMC                |
| `UPDATE_STATE_WAIT_REACHABLE`  | 3    | Waiting for switch to be reachable   |
| `UPDATE_STATE_COPY`            | 4    | Copying file to switch via SSH       |
| `UPDATE_STATE_UPLOAD`          | 5    | Uploading firmware                   |
| `UPDATE_STATE_INSTALL`         | 6    | Installing firmware                  |
| `UPDATE_STATE_POLL_COMPLETION` | 7    | Polling Redfish task for completion  |
| `UPDATE_STATE_VERIFY`          | 8    | Verifying installed version          |
| `UPDATE_STATE_CLEANUP`         | 9    | Cleaning up temp files               |
| `UPDATE_STATE_COMPLETED`       | 10   | Update finished successfully         |
| `UPDATE_STATE_FAILED`          | 11   | Update failed; check error message   |
| `UPDATE_STATE_CANCELLED`       | 12   | Cancelled due to predecessor failure |

---

## RPCs

### RegisterNVSwitches

Registers one or more NV-Switch trays by their BMC and NVOS subsystem identifiers and persists credentials for subsequent operations.

```protobuf
rpc RegisterNVSwitches(RegisterNVSwitchesRequest) returns (RegisterNVSwitchesResponse)
```

#### Request

```protobuf
message RegisterNVSwitchesRequest {
    repeated RegisterNVSwitchRequest registration_requests = 1;
}

message RegisterNVSwitchRequest {
    Vendor vendor = 1;
    Subsystem bmc = 2;        // BMC subsystem (MAC, IP, credentials, port)
    Subsystem nvos = 3;       // NVOS subsystem (MAC, IP, credentials, port)
    string rack_id = 4;       // Optional rack identifier for grouping
}

message Subsystem {
    string mac_address = 1;
    string ip_address = 2;
    Credentials credentials = 3;
    int32 port = 4;           // Optional custom port (0 = default)
}
```

#### Response

```protobuf
message RegisterNVSwitchesResponse {
    repeated RegisterNVSwitchResponse responses = 1;
}

message RegisterNVSwitchResponse {
    string uuid = 1;              // Service-generated UUID
    bool is_new = 2;              // true if newly created, false if updated
    google.protobuf.Timestamp created = 3;
    StatusCode status = 4;
    string error = 5;
}
```

#### Behavior

- Idempotent: re-registering an existing switch updates credentials
- Credentials are stored separately from device registry
- UUID is service-generated and serves as the primary identifier

#### Example

```bash
grpcurl -plaintext -d '{
  "registration_requests": [{
    "vendor": "VENDOR_NVIDIA",
    "bmc": {
      "mac_address": "00:11:22:33:44:55",
      "ip_address": "10.0.1.100",
      "credentials": {"username": "admin", "password": "secret"}
    },
    "nvos": {
      "mac_address": "00:11:22:33:44:56",
      "ip_address": "10.0.1.101",
      "credentials": {"username": "admin", "password": "secret"}
    },
    "rack_id": "rack-01"
  }]
}' localhost:50051 v1.NVSwitchManager/RegisterNVSwitches
```

---

### GetNVSwitches

Retrieves full inventory for the specified NV-Switch trays, including BMC metadata, NVOS info, and chassis details.

```protobuf
rpc GetNVSwitches(NVSwitchRequest) returns (GetNVSwitchesResponse)
```

#### Request

```protobuf
message NVSwitchRequest {
    repeated string uuids = 1;  // Switch UUIDs. Empty = return all registered
}
```

#### Response

```protobuf
message GetNVSwitchesResponse {
    repeated NVSwitchTray nvswitches = 1;
}

message NVSwitchTray {
    string uuid = 1;
    Vendor vendor = 2;
    BMCInfo bmc = 3;
    NVOSInfo nvos = 4;
    Chassis chassis = 5;
    string cpld_version = 6;
    string rack_id = 7;
}
```

#### Example

```bash
# Get all switches
grpcurl -plaintext -d '{}' localhost:50051 v1.NVSwitchManager/GetNVSwitches

# Get specific switches
grpcurl -plaintext -d '{
  "uuids": ["uuid-1", "uuid-2"]
}' localhost:50051 v1.NVSwitchManager/GetNVSwitches
```

---

### ListBundles

Returns all available firmware bundles with their component details.

```protobuf
rpc ListBundles(google.protobuf.Empty) returns (ListBundlesResponse)
```

#### Response

```protobuf
message ListBundlesResponse {
    repeated FirmwareBundle bundles = 1;
}

message FirmwareBundle {
    string version = 1;
    string description = 2;
    repeated ComponentInfo components = 3;
}

message ComponentInfo {
    string name = 1;        // Component name (firmware, cpld, nvos)
    string version = 2;
    string strategy = 3;    // Update strategy (redfish, ssh, script)
}
```

#### Example

```bash
grpcurl -plaintext -d '{}' localhost:50051 v1.NVSwitchManager/ListBundles
```

---

### QueueUpdate

Queues firmware updates for one or more components on a single switch. If components is empty, all components in the bundle are updated in sequence.

```protobuf
rpc QueueUpdate(QueueUpdateRequest) returns (QueueUpdateResponse)
```

#### Request

```protobuf
message QueueUpdateRequest {
    string switch_uuid = 1;
    string bundle_version = 2;
    repeated NVSwitchComponent components = 3;  // Empty = all
}
```

#### Response

```protobuf
message QueueUpdateResponse {
    repeated FirmwareUpdateInfo updates = 1;
}
```

#### Behavior

- Returns immediately after queueing; use `GetUpdate` or `GetUpdatesForSwitch` to poll
- When components is empty, all bundle components are queued with predecessor chaining
- Concurrent updates to the same switch/component are rejected

#### Example

```bash
grpcurl -plaintext -d '{
  "switch_uuid": "uuid-1",
  "bundle_version": "1.0.0",
  "components": ["NVSWITCH_COMPONENT_BMC"]
}' localhost:50051 v1.NVSwitchManager/QueueUpdate
```

---

### QueueUpdates

Queues firmware updates for multiple switches in a single call.

```protobuf
rpc QueueUpdates(QueueUpdatesRequest) returns (QueueUpdatesResponse)
```

#### Request

```protobuf
message QueueUpdatesRequest {
    repeated string switch_uuids = 1;
    string bundle_version = 2;
    repeated NVSwitchComponent components = 3;  // Empty = all
}
```

#### Response

```protobuf
message QueueUpdatesResponse {
    repeated QueueUpdateResult results = 1;
}

message QueueUpdateResult {
    string switch_uuid = 1;
    StatusCode status = 2;
    string error = 3;
    repeated FirmwareUpdateInfo updates = 4;
}
```

#### Example

```bash
grpcurl -plaintext -d '{
  "switch_uuids": ["uuid-1", "uuid-2"],
  "bundle_version": "1.0.0"
}' localhost:50051 v1.NVSwitchManager/QueueUpdates
```

---

### GetUpdate

Returns the status of a specific firmware update by ID.

```protobuf
rpc GetUpdate(GetUpdateRequest) returns (GetUpdateResponse)
```

#### Example

```bash
grpcurl -plaintext -d '{
  "update_id": "update-uuid"
}' localhost:50051 v1.NVSwitchManager/GetUpdate
```

---

### GetUpdatesForSwitch

Returns all firmware updates for a specific switch.

```protobuf
rpc GetUpdatesForSwitch(GetUpdatesForSwitchRequest) returns (GetUpdatesForSwitchResponse)
```

#### Example

```bash
grpcurl -plaintext -d '{
  "switch_uuid": "uuid-1"
}' localhost:50051 v1.NVSwitchManager/GetUpdatesForSwitch
```

---

### GetAllUpdates

Returns all firmware updates across all switches.

```protobuf
rpc GetAllUpdates(google.protobuf.Empty) returns (GetAllUpdatesResponse)
```

#### Example

```bash
grpcurl -plaintext -d '{}' localhost:50051 v1.NVSwitchManager/GetAllUpdates
```

---

### CancelUpdate

Cancels an in-progress firmware update.

```protobuf
rpc CancelUpdate(CancelUpdateRequest) returns (CancelUpdateResponse)
```

#### Example

```bash
grpcurl -plaintext -d '{
  "update_id": "update-uuid"
}' localhost:50051 v1.NVSwitchManager/CancelUpdate
```

---

### PowerControl

Performs a power action (e.g., PowerCycle, ForceOff, On) on one or more NV-Switch trays. Supports both registered switches (by UUID) and unregistered devices (by inline connection details via `PowerTarget`).

```protobuf
rpc PowerControl(PowerControlRequest) returns (PowerControlResponse)
```

#### Request

```protobuf
message PowerControlRequest {
    repeated string uuids = 1;          // Registered switches by UUID
    PowerAction action = 2;
    repeated PowerTarget targets = 3;   // Unregistered devices with inline credentials
}

message PowerTarget {
    string bmc_ip = 1;
    Credentials bmc_credentials = 2;
    int32 bmc_port = 3;                 // 0 = default (443)
}
```

#### Response

```protobuf
message PowerControlResponse {
    repeated NVSwitchResponse responses = 1;
}

message NVSwitchResponse {
    string uuid = 1;       // Set for registered switches; empty for direct targets
    StatusCode status = 2;
    string error = 3;
    string bmc_ip = 4;     // Set for direct PowerTarget responses; empty for registered switches
}
```

#### Behavior

- Returns per-switch status; partial failures are possible
- For registered switches, `uuid` identifies the device in the response
- For direct `PowerTarget` requests, `bmc_ip` identifies the device and `uuid` is empty

#### Example

```bash
# Registered switch by UUID
grpcurl -plaintext -d '{
  "uuids": ["uuid-1"],
  "action": "POWER_ACTION_POWER_CYCLE"
}' localhost:50051 v1.NVSwitchManager/PowerControl

# Unregistered device via PowerTarget
grpcurl -plaintext -d '{
  "action": "POWER_ACTION_FORCE_OFF",
  "targets": [{
    "bmc_ip": "10.0.1.100",
    "bmc_credentials": {"username": "admin", "password": "secret"}
  }]
}' localhost:50051 v1.NVSwitchManager/PowerControl
```

---

## Error Handling

### Partial Failures

Batch operations return individual status codes per target. Always iterate through responses:

```go
resp, err := client.RegisterNVSwitches(ctx, req)
if err != nil {
    // Transport/connection error
    return err
}

for _, r := range resp.Responses {
    if r.Status != pb.StatusCode_SUCCESS {
        log.Errorf("Failed to register %s: %s", r.Uuid, r.Error)
    }
}
```

### Common Error Scenarios

| Scenario              | StatusCode         | Error Message Pattern                         |
|-----------------------|--------------------|-----------------------------------------------|
| Invalid MAC format    | `INVALID_ARGUMENT` | `invalid MAC address: ...`                    |
| Unsupported component | `INVALID_ARGUMENT` | `unsupported component`                       |
| Switch not registered | `INTERNAL_ERROR`   | `switch not found`                            |
| Redfish unreachable   | `INTERNAL_ERROR`   | `connection refused` / `timeout`              |
| Database error        | `INTERNAL_ERROR`   | `failed to query: ...`                        |

---

## Versioning

The API follows semantic versioning. Breaking changes increment the package version (`v1` → `v2`). The current version is **v1**.

Non-breaking changes (new fields, new RPCs) are added to the existing package.
