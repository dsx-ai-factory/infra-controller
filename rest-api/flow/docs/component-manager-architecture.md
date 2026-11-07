# Component Manager Architecture

This document explains the architecture of the Component Manager system, including the Provider pattern and Factory pattern used for dependency injection and extensibility.

## Overview

The Component Manager system uses two main patterns:

1. **Provider Pattern** - Wraps API clients and manages their lifecycle
2. **Factory Pattern** - Creates component manager instances with their required dependencies

```
┌─────────────────────────────────────────────────────────────────────┐
│                         cmd/serve.go                                │
│  (Application Entry Point - Wiring & Bootstrap)                     │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      ProviderRegistry                               │
│  ┌─────────────┐  ┌─────────────┐                                  │
│  │    nico     │  │  (new...)   │                                  │
│  │  Provider   │  │  Provider   │                                  │
│  └─────────────┘  └─────────────┘                                  │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    ComponentManager Registry                        │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │ ComponentType: Compute                                       │   │
│  │   ├── "nico"       → Factory → Manager (uses nico.Provider)  │   │
│  │   ├── "nicolegacy" → Factory → Manager (uses nico.Provider)  │   │
│  │   └── "mock"       → Factory → Manager (no provider needed)  │   │
│  ├─────────────────────────────────────────────────────────────┤   │
│  │ ComponentType: NVSwitch                                      │   │
│  │   ├── "nico" → Factory → Manager (uses nico.Provider)        │   │
│  │   └── "mock" → Factory → Manager                             │   │
│  ├─────────────────────────────────────────────────────────────┤   │
│  │ ComponentType: PowerShelf                                    │   │
│  │   ├── "nico" → Factory → Manager (uses nico.Provider)        │   │
│  │   └── "mock" → Factory → Manager                             │   │
│  └─────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

## Key Components

### Provider Interface

```go
// Provider is a marker interface for API client providers.
type Provider interface {
    Name() string  // Unique identifier for this provider
}
```

Providers wrap API clients and are registered in the `providerapi.ProviderRegistry`. Component managers retrieve providers by name to get their required API clients.

### ProviderRegistry

Manages provider instances. Component manager factories use `providerapi.GetTyped[T]()` to retrieve type-safe providers:

```go
provider, err := providerapi.GetTyped[*nico.Provider](
    providerRegistry,
    nico.ProviderName,
)
```

### ComponentManager Interface

```go
type ComponentManager interface {
    Descriptor() cmcatalog.Descriptor
}

type ExpectationInjector interface {
    // CapabilityInjectExpectation
    InjectExpectation(ctx, target, info) error
}

type PowerController interface {
    // CapabilityPowerControl
    PowerControl(ctx, target, info) error
}

type PowerStatusReader interface {
    // CapabilityPowerStatus
    GetPowerStatus(ctx, target) (map, error)
}

type FirmwareController interface {
    // CapabilityFirmwareControl
    FirmwareControl(ctx, target, info) error
}

type FirmwareStatusReader interface {
    // CapabilityFirmwareStatus
    GetFirmwareStatus(ctx, target) (map, error)
}

type BringUpController interface {
    // CapabilityBringUpControl
    BringUpControl(ctx, target, info) error
}

type BringUpStatusReader interface {
    // CapabilityBringUpStatus
    GetBringUpStatus(ctx, target) (map, error)
}

type FirmwareConsistencyChecker interface {
    // CapabilityFirmwareConsistencyCheck
    VerifyFirmwareConsistency(ctx, target) error
}
```

The base interface only carries descriptor metadata. Operation methods are
capability-specific interfaces, so implementations only define the operations
they advertise in `catalog.Descriptor.Capabilities`.

### ManagerFactory

```go
type ManagerFactory func(providers *providerapi.ProviderRegistry) (ComponentManager, error)
```

Factory functions create component manager instances. They receive the `ProviderRegistry` to retrieve any required providers.

`catalog.Descriptor` contains static implementation metadata used for
configuration validation. `FactorySpec` pairs that descriptor with the runtime
factory used after service config has been loaded.

### Catalog and Registry

The `catalog.Catalog` stores validated descriptors for implementations compiled
into a service binary:
- `catalog.New()` - Validate descriptors and index them by component type and
  implementation
- `ListImplementations()` - List supported implementations by component type

The `Registry` stores active managers selected from runtime factory specs:
- `NewRegistry()` - Create managers based on configuration and the supplied
  factory specs
- `GetManager()` - Retrieve active manager for a component type, returning a
  descriptive error when the registry is not configured or no manager is active
- `GetDescriptor()` - Retrieve the descriptor selected for a component type
- `FindManager()` - Probe for an active manager, returning nil when absent

## Directory Structure

```
internal/task/componentmanager/
├── manager.go               # ComponentManager interface
├── factory_spec.go          # Manager factories and factory specs
├── registry.go              # Active component manager registry
├── providerapi/             # Provider interfaces and registries
├── config.go                # Configuration parsing
├── mock/
│   └── mock.go              # Generic mock implementation
├── providers/
│   └── nico/
│       └── provider.go      # NICo API provider
├── compute/
│   ├── nico/
│   │   └── nico.go          # Compute manager using Core's Component Manager dispatch
│   └── nicolegacy/
│       └── nicolegacy.go    # Legacy compute manager via machine-centric NICo RPCs
├── nvswitch/
│   └── nico/
│       └── nico.go          # NICo-based NVSwitch manager
└── powershelf/
    └── nico/
        └── nico.go          # NICo-based power shelf manager
```

---

## Adding a New Provider

Follow these steps to add a new API provider (e.g., a new external service).

### Step 1: Create the Provider Package

Create `internal/task/componentmanager/providers/<name>/provider.go`:

```go
package myapi

import (
    "time"
    "github.com/rs/zerolog/log"
    "github.com/NVIDIA/infra-controller/rest-api/flow/internal/myapi"  // Your API client
)

const (
    ProviderName   = "myapi"
    DefaultTimeout = 30 * time.Second
)

// Config holds configuration for the provider.
type Config struct {
    Timeout time.Duration
}

// Provider wraps the API client.
type Provider struct {
    client myapi.Client
}

// New creates a new Provider using the provided configuration.
func New(config Config) (*Provider, error) {
    client, err := myapi.NewClient(config.Timeout)
    if err != nil {
        log.Error().Err(err).Msg("Failed to create MyAPI client")
        return nil, err
    }
    return &Provider{client: client}, nil
}

// NewFromClient creates a Provider from an existing client (for testing).
func NewFromClient(client myapi.Client) *Provider {
    return &Provider{client: client}
}

// Name returns the unique identifier for this provider.
func (p *Provider) Name() string {
    return ProviderName
}

// Client returns the underlying API client.
func (p *Provider) Client() myapi.Client {
    return p.client
}
```

### Step 2: Add Configuration Support

Provider-specific configuration lives with the provider package. Add a
`ConfigDecoder` that implements the `providerapi.ProviderConfigDecoder`
interface:

```go
type ConfigDecoder struct{}

func (ConfigDecoder) Name() string {
    return ProviderName
}

func (ConfigDecoder) DefaultConfig() providerapi.ProviderConfig {
    return &Config{Timeout: DefaultTimeout}
}

func (d ConfigDecoder) DecodeYAML(raw yaml.Node) (providerapi.ProviderConfig, error) {
    config := d.DefaultConfig().(*Config)
    // Decode provider-specific YAML into config.
    return config, nil
}
```

Generic YAML parsing and validation lives in
`internal/task/componentmanager/config`. That package should not import
provider implementations directly.

Manager-specific behavior settings use the same pattern, but are keyed by
descriptor identity instead of provider name. A manager that needs YAML config
implements `cmconfig.ManagerConfigDecoder`; the decoded config is stored under
`Config.ManagerConfigs[Descriptor().Identity()]`. Provider configs should stay
focused on provider/client settings such as timeouts and endpoints.

### Step 3: Register the Provider Decoder

Update the service-supported provider catalog in
`internal/task/componentmanager/builtin`:

```go
func serviceProviderConfigDecoders() []providerapi.ProviderConfigDecoder {
    return []providerapi.ProviderConfigDecoder{
        nico.ConfigDecoder{},
        myapi.ConfigDecoder{},
    }
}
```

`cmd/serve.go` does not need provider-specific construction code. It loads the
service config through `builtin.LoadConfig`, then creates providers from the
decoded generic provider configs. Built-in provider decoders and component
manager factory specs both live in `internal/task/componentmanager/builtin`.

---

## Adding a New Component Manager Implementation

Follow these steps to add a new implementation for an existing component type.

### Step 1: Create the Implementation Package

Create `internal/task/componentmanager/<component_type>/<impl_name>/<impl_name>.go`:

```go
package myimpl

import (
    "context"
    "fmt"

    "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
    "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
    cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
    "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
    myapiprovider "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providers/myapi"
    "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
    "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
    "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

const ImplementationName = "myimpl"

// Manager implements ComponentManager using MyAPI.
type Manager struct {
    client myapi.Client
}

// New creates a new Manager instance.
func New(client myapi.Client) *Manager {
    return &Manager{client: client}
}

// Factory creates a Manager from the ProviderRegistry.
func Factory(providers *providerapi.ProviderRegistry) (componentmanager.ComponentManager, error) {
    provider, err := providerapi.GetTyped[*myapiprovider.Provider](
        providers,
        myapiprovider.ProviderName,
    )
    if err != nil {
        return nil, fmt.Errorf("myimpl requires myapi provider: %w", err)
    }
    return New(provider.Client()), nil
}

// Descriptor returns this implementation's descriptor.
func Descriptor() cmcatalog.Descriptor {
    return cmcatalog.Descriptor{
        Type:              devicetypes.ComponentTypeCompute,
        Implementation:    ImplementationName,
        RequiredProviders: []string{myapiprovider.ProviderName},
        Capabilities: capability.CapabilitySet{
            capability.CapabilityInjectExpectation,
            capability.CapabilityPowerControl,
            capability.CapabilityFirmwareControl,
        },
    }
}

// FactorySpec returns this implementation's runtime factory spec.
func FactorySpec() componentmanager.FactorySpec {
    return componentmanager.FactorySpec{
        Descriptor: Descriptor(),
        Factory:    Factory,
    }
}

// Descriptor returns this implementation's descriptor.
func (m *Manager) Descriptor() cmcatalog.Descriptor {
    return Descriptor()
}

// InjectExpectation implements componentmanager.ExpectationInjector.
func (m *Manager) InjectExpectation(ctx context.Context, target common.Target, info operations.InjectExpectationTaskInfo) error {
    // Implementation here
}

// PowerControl implements componentmanager.PowerController.
func (m *Manager) PowerControl(ctx context.Context, target common.Target, info operations.PowerControlTaskInfo) error {
    // Implementation here
}

// FirmwareControl implements componentmanager.FirmwareController.
func (m *Manager) FirmwareControl(ctx context.Context, target common.Target, info operations.FirmwareControlTaskInfo) error {
    // Implementation here — initiate firmware update, return immediately
}
```

### Step 2: Register the Implementation

Update the service-supported manager catalog in
`internal/task/componentmanager/builtin`:

```go
import (
    "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
    cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
    cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
    myimpl "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/compute/myimpl"
)

func serviceDescriptors() []cmcatalog.Descriptor {
    descriptors := []cmcatalog.Descriptor{
        // ... existing descriptors ...
        myimpl.Descriptor(), // Add new implementation
    }
    return descriptors
}

func serviceFactorySpecs(config cmconfig.Config) ([]componentmanager.FactorySpec, error) {
    // Simplified example: production code should check that the config exists
    // and has the expected type, returning an error instead of panicking.
    cfg := config.ManagerConfigs[myimpl.Descriptor().Identity()].(*myimpl.Config)
    factorySpecs := []componentmanager.FactorySpec{
        // ... existing factory specs ...
        myimpl.FactorySpec(cfg), // Add new implementation
    }
    return factorySpecs, nil
}
```

### Step 3: Use in Configuration

Now you can use the new implementation in YAML config:

```yaml
component_managers:
  compute: myimpl
  nvswitch: nico
  powershelf: nico

manager_configs:
  compute:
    myimpl:
      behavior_setting: "value"

providers:
  myapi:
    timeout: "30s"
  nico:
    timeout: "1m"
```

---

## Adding a New Component Type

To add an entirely new component type (e.g., `gpu`):

1. Add the type to `pkg/common/devicetypes/component.go`
2. Create implementation(s) under `internal/task/componentmanager/gpu/<impl>/`
3. Update the mock in `internal/task/componentmanager/mock/mock.go` to include a descriptor for it
4. Update configuration parsing to recognize the new type

---

## Testing

### Unit Testing with Mock Providers

```go
func TestManager(t *testing.T) {
    mockClient := &MockMyAPIClient{}
    manager := myimpl.New(mockClient)
    
    err := manager.PowerControl(ctx, target, info)
    assert.NoError(t, err)
}
```

### Integration Testing with Mock Implementation

Use the mock implementation in test configuration:

```yaml
component_managers:
  compute: mock
  nvswitch: mock
  powershelf: mock
```
