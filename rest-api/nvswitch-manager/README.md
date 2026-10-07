# NV-Switch Manager

NV-Switch Manager is a gRPC service for managing NVIDIA DGX GB200 NVLink Switch Trays in datacenters. The service provides a control plane to register devices, manage credentials securely, query inventory, control power state, and orchestrate firmware upgrades for BMC, CPLD, BIOS, and NVOS components.

## At-a-Glance

1. gRPC API: internal/proto/v1
2. Orchestration: pkg/nvswitchmanager
3. Redfish access: pkg/redfish (thin wrapper around gofish)
4. Firmware management: pkg/firmwaremanager (worker pool, upgrade strategies, update tracking)
5. Registry: pkg/nvswitchregistry (Postgres or InMemory), pkg/db (Bun ORM + pgx)
6. Credentials: pkg/credentials (Vault KV or InMemory)

## Architecture Overview
The service is layered with clear separation of responsibilities:

1. API (gRPC) — internal/service
    1. NVSwitchManagerServer implements RPCs for device registration, inventory queries, power control, and firmware management.
    2. Protobuf schema in internal/proto/v1 encapsulates the public service surface.
2. Orchestration — pkg/nvswitchmanager
    1. Central coordinator that wires the NV-Switch registry, credential manager, firmware manager, and Redfish/SSH client sessions per request.
    2. Stateless at the orchestration layer; state is delegated to backends.
3. Device Access — pkg/redfish, pkg/sshclient
    1. Encapsulates Redfish operations (query chassis/manager, power actions, firmware upload).
    2. SSH client for NVOS-level operations.
4. Firmware Management — pkg/firmwaremanager
    1. Background worker pool with configurable concurrency.
    2. Multiple update strategies (SSH, Redfish, Script).
    3. State machine: QUEUED → POWER_CYCLE → COPY → UPLOAD → INSTALL → VERIFY → COMPLETED/FAILED.
    4. Upgrade execution with PostgreSQL-backed update tracking.
5. NV-Switch Registry — pkg/nvswitchregistry
    1. Stores NV-Switch tray identity and routing attributes (MAC, IP, vendor, rack ID).
    2. Implementations: Postgres (prod), InMemory (dev/tests).
    3. Authoritative source of device inventory for the service.
6. Secrets: Credential Manager — pkg/credentials
    1. Stores and retrieves per-device credentials keyed by MAC address.
    2. Implementations: Vault KV v2 (prod), InMemory (dev/tests).
    3. Explicitly separated from the device registry to isolate secret material.

This architecture emphasizes stateless orchestration at the service layer (driven by gRPC), separation of concerns for identity (device registry) and secrets (credential manager), firmware lifecycle management with background workers and upgrade strategies, and a clean boundary to device access through Redfish and SSH client wrappers. The design favors idempotency where possible, supports both in-memory and persistent backends, and treats firmware as a first-class workflow with update tracking and well-defined error semantics.

## gRPC API
Service definition: internal/proto/v1/nvswitch-manager.proto

## Local Development

This section provides a repeatable local development workflow using Docker Compose and helper scripts. It stands up Postgres, runs database migrations, starts the gRPC service, and verifies via grpcui. It is service-focused and assumes you are iterating on the NV-Switch Manager server.

### Prerequisites
1. Docker and Docker Compose
2. Go toolchain (1.26.4+)
3. grpcui (optional) to exercise the gRPC API
4. psql client (optional) for DB inspection

### 1. Start local infrastructure (Postgres)
```
docker compose up -d
```

### 2. Build the service binary

```
go build -o nvswitch-manager
```

### 3. Run DB migrations (create/drop tables)
```
# create initial tables
./nvswitch-manager migrate --host localhost --port 5432 --dbname nsmdatabase --user nsmuser --password nsmpassword

# roll back (drop tables)
./nvswitch-manager migrate --host localhost --port 5432 --dbname nsmdatabase --user nsmuser --password nsmpassword --rollback
```

### 4. Start the NV-Switch Manager gRPC service
Run with a persistent backend (Postgres + Vault):

```
# minimal (defaults)
./nvswitch-manager serve -d Persistent

# explicit flags
./nvswitch-manager serve \
  --datastore Persistent \
  --port 50051 \
  --db_user nsmuser \
  --db_password nsmpassword \
  --db_port 5432 \
  --db_host localhost \
  --db_name nsmdatabase \
  --vault_token nsmvaultroot \
  --vault_address http://127.0.0.1:8201

# short flags
./nvswitch-manager serve \
  -d Persistent \
  -p 50051 \
  -u nsmuser \
  -b nsmpassword \
  -r 5432 \
  -o localhost \
  -n nsmdatabase \
  -t nsmvaultroot \
  -a http://127.0.0.1:8201
```

### 5. Exercise the API via grpcui
```
grpcui -plaintext localhost:50051
```
