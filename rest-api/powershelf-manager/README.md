# Powershelf Manager

Powershelf Manager is a gRPC service for managing power shelves in datacenters via their Power Management Controllers (PMCs) using the Redfish API. The service provides a control plane to register devices, manage credentials securely, query inventory and telemetry, control power state, and orchestrate firmware upgrades. 

## At-a-Glance

1. gRPC API: internal/proto/v1
2. Orchestration: pkg/powershelfmanager
3. Redfish access: pkg/redfish (thin wrapper around gofish)
4. Firmware management: pkg/firmwaremanager (embedded repo, upgrade rules, uploader)
5. Registry: pkg/pmcregistry (Postgres or InMemory), pkg/db (Bun ORM + pgx)
6. Credentials: pkg/credentials (Vault KV or InMemory)

## Architecture Overview
The service is layered with clear separation of responsibilities:

1. API (gRPC) — internal/service
    1. PowershelfManagerServer implements RPCs for PMC registration, inventory queries, power control, and firmware management.
    2. Protobuf schema in internal/proto/v1 encapsulates the public service surface.
2. Orchestration — pkg/powershelfmanager
    1. Central coordinator that wires the PMC registry, credential manager, firmware manager, and Redfish client sessions per request.
    2. Stateless at the orchestration layer; state is delegated to backends.
3. Device Access — pkg/redfish
    1. Encapsulates Redfish operations (query chassis/manager/PSUs, power actions, firmware upload).
    2. Wraps gofish for typed Redfish navigation and adds targeted HTTP flows where needed (e.g., UpdateService uploads).
4. Firmware Management — pkg/firmwaremanager
    1. Embedded firmware assets (Go embed) organized by vendor.
    2. Parsing of upgrade edges from artifact names.
    3. Vendor-specific UpgradeRule (Liteon: direct-only).
    4. Upgrade execution via Redfish UpdateService with optional dry-run.
5. PMC Registry — pkg/pmcregistry
    1. Stores non-sensitive PMC identity and routing attributes (MAC, IP, vendor).
    2. Implementations: Postgres (prod), InMemory (dev/tests).
    3. This is the authoritative source of device inventory for the service.
6. Secrets: Credential Manager — pkg/credentials
    1. Stores and retrieves per-PMC credentials keyed by MAC address.
    2. Implementations: Vault KV v2 (prod), InMemory (dev/tests).
    3. Explicitly separated from the PMC registry to isolate secret material.

This architecture emphasizes stateless orchestration at the service layer (driven by gRPC), separation of concerns for identity (PMC registry) and secrets (credential manager), vendor-aware firmware lifecycle management with embedded artifacts and upgrade policies, and a clean boundary to device access through a thin Redfish client wrapper. The design favors idempotency where possible (e.g., registration and firmware checks), supports both in-memory and persistent backends to cover local development and production, and treats firmware as a first-class workflow with dry-run support, upgrade rules, and well-defined error semantics.

## gRPC API
Service definition: internal/proto/v1/powershelf-manager.proto

RPCs:

1. RegisterPmc(RegisterPmcRequest) → RegisterPmcResponse
2. IsPmcRegistered(PmcRequest) → IsPmcRegisteredResponse
3. GetPowershelf(PmcRequest) → GetPowershelfResponse
4. GetAllPowershelves(google.protobuf.Empty) → GetAllPowershelvesResponse
5. CanUpdateFirmware(PmcRequest) → CanUpdateFirmwareResponse
6. UpdateFirmware(UpdateFirmwareRequest) → UpdateFirmwareResponse
7. PowerOff(PmcRequest) → google.protobuf.Empty
8. PowerOn(PmcRequest) → google.protobuf.Empty

## Local Development

This section provides a repeatable local development workflow using Docker Compose and helper scripts. It stands up Postgres and Vault, runs database migrations, starts the gRPC service, and verifies via grpcui. It is service-focused and assumes you are iterating on the Powershelf Manager server.

### Prerequisites
1. Docker and Docker Compose
2. Go toolchain (1.21+ recommended)
3. grpcui (optional) to exercise the gRPC API
4. psql client (optional) for DB inspection


### 1. Start local infrastructure (Postgres + Vault)
```
docker compose up -d
```

### 2. Build the service binary

```
go build -o psm
```

### 3. Run DB migrations (create/drop tables)
```
# create initial tables
./psm migrate --host localhost --port 5432 --dbname psmdatabase --user psmuser --password psmpassword

# roll back (drop tables)
./psm migrate --host localhost --port 5432 --dbname psmdatabase --user psmuser --password psmpassword --rollback
```

### 4. Start the Powershelf Manager gRPC service
Run with a persistent backend (Postgres + Vault):

```
# minimal (defaults)
./psm serve -d Persistent

# explicit flags
./psm serve \
  --datastore Persistent \
  --port 50051 \
  --db_user psmuser \
  --db_password psmpassword \
  --db_port 5432 \
  --db_host localhost \
  --db_name psmdatabase \
  --vault_token psmvaultroot \
  --vault_address http://127.0.0.1:8201

# short flags
./psm serve \
  -d Persistent \
  -p 50051 \
  -u psmuser \
  -b psmpassword \
  -r 5432 \
  -o localhost \
  -n psmdatabase \
  -t psmvaultroot \
  -a http://127.0.0.1:8201
```

### 6. Exercise the API via grpcui
```
grpcui -plaintext localhost:50051
```
