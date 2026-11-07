# Simple SDK

The Simple SDK provides a simplified, high-level interface to the NVIDIA Infrastructure Controller (NICo) REST API. It wraps the [standard SDK](../standard) with a cleaner API and automatic metadata management.

## Features

- **Simplified types**: Clean request/response structs without OpenAPI-generated boilerplate
- **Automatic metadata**: Site, VPC, Subnet, and VPC Prefix are automatically discovered and cached

## Build

The simple SDK depends on the [standard SDK](../standard), which is generated from the OpenAPI spec.

1. **Generate the standard SDK** (from the repo root):

   ```bash
   make generate-sdk
   ```

   Requires [openapi-generator](https://openapi-generator.tech/) (e.g. `brew install openapi-generator`). To use a different spec:

   ```bash
   OPENAPI_SPEC=/path/to/spec.yaml make generate-sdk
   ```

2. **Build the simple SDK**:

   ```bash
   go build ./sdk/simple/...
   ```

## Use

### Add as a dependency

In your project's `go.mod`:

```bash
go get github.com/NVIDIA/infra-controller/rest-api/sdk/simple
```

For local development, use a `replace` directive:

```go
replace github.com/NVIDIA/infra-controller/rest-api => /path/to/infra-controller/rest-api
```

### Local development (kind)

After running `make kind-reset` from the repo root, the API is available at `http://localhost:8388` and Keycloak at `http://localhost:8082`. Use the `test-org` organization with a token from Keycloak.

**1. Get a token** (requires `jq`; run in a separate terminal or before your program):

```bash
export NICO_TOKEN=$(curl -s -X POST "http://localhost:8082/realms/nico-dev/protocol/openid-connect/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "client_id=nico-api" \
  -d "client_secret=nico-local-secret" \
  -d "grant_type=password" \
  -d "username=admin@example.com" \
  -d "password=adminpassword" | jq -r .access_token)

export NICO_BASE_URL="http://localhost:8388"
export NICO_ORG="test-org"
```

**2. Use the SDK with environment variables:**

```go
package main

import (
    "context"
    "fmt"
    "log"

    "github.com/NVIDIA/infra-controller/rest-api/sdk/simple"
)

func main() {
    client, err := simple.NewClientFromEnv()
    if err != nil {
        log.Fatal(err)
    }

    ctx := context.Background()
    if err := client.Authenticate(ctx); err != nil {
        log.Fatal(err)
    }

    // List VPCs
    vpcs, pagination, apiErr := client.GetVpcs(ctx, nil, nil)
    if apiErr != nil {
        log.Fatal(apiErr)
    }
    fmt.Printf("Found %d VPCs\n", len(vpcs))
    for _, vpc := range vpcs {
        fmt.Printf("  - %s (%s)\n", vpc.Name, vpc.ID)
    }
    _ = pagination

    // List machines
    machines, _, apiErr := client.GetMachines(ctx, nil)
    if apiErr != nil {
        log.Fatal(apiErr)
    }
    fmt.Printf("Found %d machines\n", len(machines))
}
```

Save as `main.go` in a directory with `go.mod` (or use a `replace` directive to point at this repo), then run:

```bash
go run .
```

**Or run an example** (from repo root, after `make kind-reset` and port-forwarding):

```bash
make test-simple-sdk-example   # runs machine example
go run ./sdk/simple/examples/vpc/
go run ./sdk/simple/examples/vpc/manage/      # full VPC CRUD
go run ./sdk/simple/examples/instance/
go run ./sdk/simple/examples/instance/create/  # create and delete instance
go run ./sdk/simple/examples/instance/filter_by_name/
go run ./sdk/simple/examples/instance/multi_vpc/
go run ./sdk/simple/examples/ipblock/
go run ./sdk/simple/examples/expectedmachine/
go run ./sdk/simple/examples/expectedmachine/batch_manage/
```

Examples verify the SDK can talk to the stack (list, get, create, update, delete). No build or setup required beyond a running kind cluster with API and Keycloak port-forwarded.

**3. Use the SDK with programmatic configuration:**

```go
package main

import (
    "context"
    "fmt"
    "log"

    "github.com/NVIDIA/infra-controller/rest-api/sdk/simple"
)

func main() {
    token := "your-jwt-from-keycloak" // Get via curl as shown above
    client, err := simple.NewClient(simple.ClientConfig{
        BaseURL: "http://localhost:8388",
        Org:     "test-org",
        Token:   token,
    })
    if err != nil {
        log.Fatal(err)
    }

    ctx := context.Background()
    if err := client.Authenticate(ctx); err != nil {
        log.Fatal(err)
    }

    // List machines
    machines, _, apiErr := client.GetMachines(ctx, nil)
    if apiErr != nil {
        log.Fatal(apiErr)
    }
    fmt.Printf("Found %d machines\n", len(machines))
}
```

**Test users** (from main README):

| Email | Password | Roles |
|-------|----------|-------|
| `admin@example.com` | `adminpassword` | PROVIDER_ADMIN, TENANT_ADMIN |
| `testuser@example.com` | `testpassword` | TENANT_ADMIN |
| `provider@example.com` | `providerpassword` | PROVIDER_ADMIN |

### Programmatic configuration

```go
import (
    "github.com/NVIDIA/infra-controller/rest-api/sdk/simple"
)

client, err := simple.NewClient(simple.ClientConfig{
    BaseURL: "https://api.example.com",
    Org:     "my-org",
    Token:   "your-jwt-token",
})
if err != nil {
    log.Fatal(err)
}

// Authenticate to fetch metadata (Site, VPC, etc.)
if err := client.Authenticate(ctx); err != nil {
    log.Fatal(err)
}

// Use the client
vpcs, pagination, apiErr := client.GetVpcs(ctx, nil, nil)
```

### Environment variables

Use `NewClientFromEnv()` to create a client from environment variables:

| Variable | Description |
|----------|-------------|
| `NICO_BASE_URL` | API base URL (e.g. `http://localhost:8388` for kind, `https://api.example.com` for production) |
| `NICO_ORG` | Organization name (e.g. `test-org` for kind) |
| `NICO_TOKEN` | JWT token (or `NICO_API_KEY`) |

```go
client, err := simple.NewClientFromEnv()
if err != nil {
    log.Fatal(err)
}
```
