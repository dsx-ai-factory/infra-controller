// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package builtin

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	computenico "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/compute/nico"
	computenicolegacy "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/compute/nicolegacy"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/mock"
	nvswitchnico "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/nvswitch/nico"
	powershelfnico "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/powershelf/nico"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	nicoprovider "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providers/nico"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

type testProviderConfig struct {
	name string
}

func (c testProviderConfig) Name() string {
	return c.name
}

func (c testProviderConfig) NewProvider(context.Context) (providerapi.Provider, error) {
	return nil, nil
}

type testManagerConfig struct{}

func (c testManagerConfig) Validate(cmcatalog.DescriptorIdentity) error {
	return nil
}

type testServiceProvider struct {
	name string
}

func (p testServiceProvider) Name() string {
	return p.name
}

type testServiceProviderConfig struct {
	name         string
	providerName string
	err          error
	nilProvider  bool
}

func (c testServiceProviderConfig) Name() string {
	return c.name
}

func (c testServiceProviderConfig) NewProvider(context.Context) (providerapi.Provider, error) {
	if c.err != nil {
		return nil, c.err
	}
	if c.nilProvider {
		return nil, nil
	}

	name := c.providerName
	if name == "" {
		name = c.name
	}
	return testServiceProvider{name: name}, nil
}

func TestDefaultServiceComponentManagers(t *testing.T) {
	componentManagers := defaultServiceComponentManagers()

	assert.Equal(t, computenico.ImplementationName, componentManagers[devicetypes.ComponentTypeCompute])
	assert.Equal(t, nvswitchnico.ImplementationName, componentManagers[devicetypes.ComponentTypeNVSwitch])
	assert.Equal(t, powershelfnico.ImplementationName, componentManagers[devicetypes.ComponentTypePowerShelf])

	componentManagers[devicetypes.ComponentTypeCompute] = "mutated"
	assert.Equal(
		t,
		computenico.ImplementationName,
		defaultServiceComponentManagers()[devicetypes.ComponentTypeCompute],
	)
}

func TestLoadConfigUsesDefaultsWithoutPath(t *testing.T) {
	config, err := LoadConfig("")
	require.NoError(t, err)

	assert.Equal(
		t,
		defaultServiceComponentManagers(),
		config.ComponentManagers,
	)
	assert.True(t, config.HasProvider(nicoprovider.ProviderName))

	nicoConfig, ok := config.ProviderConfigs[nicoprovider.ProviderName].(*nicoprovider.Config)
	require.True(t, ok)
	assert.Equal(t, nicoprovider.DefaultTimeout, nicoConfig.Timeout)

	// compute/nico has no manager-specific config decoder; nicolegacy's
	// manager config is only completed when that implementation is selected.
	_, hasLegacyComputeConfig := config.ManagerConfigs[computenicolegacy.Descriptor().Identity()]
	assert.False(t, hasLegacyComputeConfig)
}

func TestLoadConfigUsesAuthoritativeFile(t *testing.T) {
	path := writeServiceConfig(t, `
component_managers:
  compute: mock
providers: {}
`)

	config, err := LoadConfig(path)
	require.NoError(t, err)

	assert.Equal(t, "mock", config.ComponentManagers[devicetypes.ComponentTypeCompute])
	assert.Empty(t, config.ProviderConfigs)
	assert.False(t, config.HasProvider(nicoprovider.ProviderName))
}

func TestLoadConfigRequiresComponentManagers(t *testing.T) {
	path := writeServiceConfig(t, `
providers: {}
`)

	config, err := LoadConfig(path)

	require.Empty(t, config.ComponentManagers)
	require.Error(t, err)
	assert.True(t, errors.Is(err, cmconfig.ErrComponentManagersNotConfigured))
}

func TestLoadConfigCompletesMissingProviders(t *testing.T) {
	path := writeServiceConfig(t, `
component_managers:
  compute: nicolegacy
providers: {}
`)

	config, err := LoadConfig(path)

	require.NoError(t, err)
	assert.Equal(t, computenicolegacy.ImplementationName, config.ComponentManagers[devicetypes.ComponentTypeCompute])
	assert.True(t, config.HasProvider(nicoprovider.ProviderName))
}

func TestLoadConfigUsesExplicitManagerConfig(t *testing.T) {
	path := writeServiceConfig(t, `
component_managers:
  compute: nicolegacy
manager_configs:
  compute:
    nicolegacy:
      compute_power_delay: 0s
providers: {}
`)

	config, err := LoadConfig(path)

	require.NoError(t, err)
	computeConfig, ok := config.ManagerConfigs[computenicolegacy.Descriptor().Identity()].(*computenicolegacy.Config)
	require.True(t, ok)
	assert.Equal(t, 0*time.Second, computeConfig.ComputePowerDelay)
}

func TestNewProviderRegistry(t *testing.T) {
	t.Run("validation errors", testNewProviderRegistryErrors)
	t.Run("success", func(t *testing.T) {
		registry, err := NewProviderRegistry(
			context.Background(),
			cmconfig.Config{
				ProviderConfigs: map[string]providerapi.ProviderConfig{
					"alpha": testServiceProviderConfig{name: "alpha"},
					"beta":  testServiceProviderConfig{name: "beta"},
				},
			},
		)

		require.NoError(t, err)
		assert.ElementsMatch(t, []string{"alpha", "beta"}, registry.List())
		assert.True(t, registry.Has("alpha"))
		assert.True(t, registry.Has("beta"))
		require.NoError(t, registry.Close())
	})
	for _, mismatch := range []bool{false, true} {
		name := "construction fails after creating a provider"
		if mismatch {
			name = "created provider has wrong name"
		}
		t.Run(name, func(t *testing.T) {
			var created, closed int
			createErr := errors.New("construction failed")
			configs := make(map[string]providerapi.ProviderConfig)
			for _, providerName := range []string{"alpha", "beta"} {
				configs[providerName] = cleanupTestConfig{name: providerName, create: func() (providerapi.Provider, error) {
					created++
					if created == 2 && !mismatch {
						return nil, createErr
					}
					name := providerName
					if created == 2 {
						name = "wrong"
					}
					return cleanupTestProvider{name: name, close: func() { closed++ }}, nil
				}}
			}
			registry, err := NewProviderRegistry(context.Background(), cmconfig.Config{ProviderConfigs: configs})
			require.Nil(t, registry)
			if mismatch {
				require.ErrorIs(t, err, providerapi.ErrProviderNameMismatch)
				assert.Equal(t, 2, closed)
			} else {
				require.ErrorIs(t, err, createErr)
				assert.Equal(t, 1, closed)
			}
		})
	}
}

type cleanupTestConfig struct {
	name   string
	create func() (providerapi.Provider, error)
}

func (c cleanupTestConfig) Name() string { return c.name }
func (c cleanupTestConfig) NewProvider(context.Context) (providerapi.Provider, error) {
	return c.create()
}

type cleanupTestProvider struct {
	name  string
	close func()
}

func (p cleanupTestProvider) Name() string { return p.name }
func (p cleanupTestProvider) Close() error { p.close(); return nil }

func testNewProviderRegistryErrors(t *testing.T) {
	rootErr := errors.New("boom")

	tests := []struct {
		name      string
		config    cmconfig.Config
		wantErr   error
		checkFunc func(*testing.T, error)
	}{
		{
			name: "nil provider config",
			config: cmconfig.Config{
				ProviderConfigs: map[string]providerapi.ProviderConfig{
					"alpha": nil,
				},
			},
			wantErr: providerapi.ErrProviderNotConfigured,
			checkFunc: func(t *testing.T, err error) {
				t.Helper()
				var providerErr providerapi.ProviderNotConfiguredError
				require.True(t, errors.As(err, &providerErr))
				assert.Equal(t, "alpha", providerErr.Name)
			},
		},
		{
			name: "config name mismatch",
			config: cmconfig.Config{
				ProviderConfigs: map[string]providerapi.ProviderConfig{
					"alpha": testServiceProviderConfig{name: "other"},
				},
			},
			wantErr: providerapi.ErrProviderConfigNameMismatch,
		},
		{
			name: "provider creation failed",
			config: cmconfig.Config{
				ProviderConfigs: map[string]providerapi.ProviderConfig{
					"alpha": testServiceProviderConfig{name: "alpha", err: rootErr},
				},
			},
			wantErr: rootErr,
		},
		{
			name: "nil provider",
			config: cmconfig.Config{
				ProviderConfigs: map[string]providerapi.ProviderConfig{
					"alpha": testServiceProviderConfig{
						name:        "alpha",
						nilProvider: true,
					},
				},
			},
			wantErr: providerapi.ErrProviderNotConfigured,
		},
		{
			name: "provider name mismatch",
			config: cmconfig.Config{
				ProviderConfigs: map[string]providerapi.ProviderConfig{
					"alpha": testServiceProviderConfig{
						name:         "alpha",
						providerName: "other",
					},
				},
			},
			wantErr: providerapi.ErrProviderNameMismatch,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			registry, err := NewProviderRegistry(context.Background(), tt.config)

			require.Nil(t, registry)
			require.Error(t, err)
			assert.True(t, errors.Is(err, tt.wantErr))
			if tt.checkFunc != nil {
				tt.checkFunc(t, err)
			}
		})
	}
}

func TestNewComponentManagerRegistryInitializesBuiltInMockManagers(t *testing.T) {
	config := cmconfig.Config{
		ComponentManagers: map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypeCompute:    mock.ImplementationName,
			devicetypes.ComponentTypeNVSwitch:   mock.ImplementationName,
			devicetypes.ComponentTypePowerShelf: mock.ImplementationName,
		},
	}

	registry, err := NewComponentManagerRegistry(
		config,
		providerapi.NewProviderRegistry(),
		nil,
	)

	require.NoError(t, err)
	require.NotNil(t, registry)

	for componentType := range config.ComponentManagers {
		manager, err := registry.GetManager(componentType)
		require.NoError(t, err)
		assert.Equal(t, componentType, manager.Descriptor().Type)
	}
}

func TestServiceProviderConfigDecoderRegistry(t *testing.T) {
	registry, err := newProviderDecoderRegistry()
	require.NoError(t, err)

	assert.ElementsMatch(
		t,
		[]string{
			nicoprovider.ProviderName,
		},
		registry.List(),
	)

	_, ok := registry.Get(nicoprovider.ProviderName)
	assert.True(t, ok)
}

func TestServiceManagerConfigDecoderRegistry(t *testing.T) {
	registry, err := newManagerConfigDecoderRegistry()
	require.NoError(t, err)

	assert.Equal(
		t,
		[]cmcatalog.DescriptorIdentity{computenicolegacy.Descriptor().Identity()},
		registry.List(),
	)

	assert.NotNil(t, registry.Get(computenicolegacy.Descriptor().Identity()))
}

func TestServiceCatalog(t *testing.T) {
	catalog, err := newCatalog()

	require.NoError(t, err)

	implementations := catalog.ListImplementations()
	assert.Equal(
		t,
		[]string{
			mock.ImplementationName,
			computenico.ImplementationName,
			computenicolegacy.ImplementationName,
		},
		implementations[devicetypes.ComponentTypeCompute],
	)
	assert.Equal(
		t,
		[]string{
			mock.ImplementationName,
			nvswitchnico.ImplementationName,
		},
		implementations[devicetypes.ComponentTypeNVSwitch],
	)
	assert.Equal(
		t,
		[]string{
			mock.ImplementationName,
			powershelfnico.ImplementationName,
		},
		implementations[devicetypes.ComponentTypePowerShelf],
	)

	tests := []struct {
		name              string
		componentType     devicetypes.ComponentType
		implementation    string
		requiredProviders []string
		capabilities      capability.CapabilitySet
	}{
		{
			name:              "compute nico",
			componentType:     devicetypes.ComponentTypeCompute,
			implementation:    computenico.ImplementationName,
			requiredProviders: []string{nicoprovider.ProviderName},
			capabilities: capability.CapabilitySet{
				capability.CapabilityBringUpControl,
				capability.CapabilityBringUpStatus,
				capability.CapabilityDecommissionControl,
				capability.CapabilityDecommissionStatus,
				capability.CapabilityFirmwareControl,
				capability.CapabilityFirmwareStatus,
				capability.CapabilityInjectExpectation,
				capability.CapabilityPowerControl,
				capability.CapabilityPowerStatus,
			},
		},
		{
			name:              "compute nicolegacy",
			componentType:     devicetypes.ComponentTypeCompute,
			implementation:    computenicolegacy.ImplementationName,
			requiredProviders: []string{nicoprovider.ProviderName},
			capabilities: capability.CapabilitySet{
				capability.CapabilityBringUpControl,
				capability.CapabilityBringUpStatus,
				capability.CapabilityFirmwareControl,
				capability.CapabilityFirmwareStatus,
				capability.CapabilityInjectExpectation,
				capability.CapabilityPowerControl,
				capability.CapabilityPowerStatus,
			},
		},
		{
			name:              "nvswitch nico",
			componentType:     devicetypes.ComponentTypeNVSwitch,
			implementation:    nvswitchnico.ImplementationName,
			requiredProviders: []string{nicoprovider.ProviderName},
			capabilities: capability.CapabilitySet{
				capability.CapabilityDecommissionControl,
				capability.CapabilityDecommissionStatus,
				capability.CapabilityFirmwareConsistencyCheck,
				capability.CapabilityFirmwareControl,
				capability.CapabilityFirmwareStatus,
				capability.CapabilityInjectExpectation,
				capability.CapabilityPowerControl,
				capability.CapabilityPowerStatus,
			},
		},
		{
			name:              "powershelf nico",
			componentType:     devicetypes.ComponentTypePowerShelf,
			implementation:    powershelfnico.ImplementationName,
			requiredProviders: []string{nicoprovider.ProviderName},
			capabilities: capability.CapabilitySet{
				capability.CapabilityDecommissionControl,
				capability.CapabilityDecommissionStatus,
				capability.CapabilityFirmwareControl,
				capability.CapabilityFirmwareStatus,
				capability.CapabilityInjectExpectation,
				capability.CapabilityPowerControl,
				capability.CapabilityPowerStatus,
			},
		},
		{
			name:           "compute mock",
			componentType:  devicetypes.ComponentTypeCompute,
			implementation: mock.ImplementationName,
			capabilities:   mockCapabilities(),
		},
		{
			name:           "nvswitch mock",
			componentType:  devicetypes.ComponentTypeNVSwitch,
			implementation: mock.ImplementationName,
			capabilities:   mockCapabilities(),
		},
		{
			name:           "powershelf mock",
			componentType:  devicetypes.ComponentTypePowerShelf,
			implementation: mock.ImplementationName,
			capabilities:   mockCapabilities(),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			descriptor := requireDescriptor(
				t,
				catalog,
				tt.componentType,
				tt.implementation,
			)
			assert.ElementsMatch(t, tt.requiredProviders, descriptor.RequiredProviders)
			assertDescriptorCapabilities(t, descriptor, tt.capabilities...)
		})
	}
}

func TestLegacyComputePowerDelayUsesManagerConfig(t *testing.T) {
	delay := 7 * time.Second
	config := cmconfig.Config{
		ManagerConfigs: map[cmcatalog.DescriptorIdentity]cmconfig.ManagerConfig{
			computenicolegacy.Descriptor().Identity(): &computenicolegacy.Config{
				ComputePowerDelay: delay,
			},
		},
	}

	got, err := legacyComputePowerDelay(config)

	require.NoError(t, err)
	assert.Equal(t, delay, got)
}

func TestLegacyComputePowerDelayDefaultsWhenManagerConfigMissing(t *testing.T) {
	got, err := legacyComputePowerDelay(cmconfig.Config{})

	require.NoError(t, err)
	assert.Equal(t, computenicolegacy.DefaultComputePowerDelay, got)
}

func TestLegacyComputePowerDelayRejectsUnexpectedConfigType(t *testing.T) {
	config := cmconfig.Config{
		ManagerConfigs: map[cmcatalog.DescriptorIdentity]cmconfig.ManagerConfig{
			computenicolegacy.Descriptor().Identity(): testManagerConfig{},
		},
	}

	got, err := legacyComputePowerDelay(config)

	assert.Equal(t, time.Duration(0), got)
	require.Error(t, err)
	assert.True(t, errors.Is(err, componentmanager.ErrManagerConfigTypeMismatch))

	var mismatch componentmanager.ManagerConfigTypeMismatchError
	require.True(t, errors.As(err, &mismatch))
	assert.Equal(t, computenicolegacy.Descriptor().Identity(), mismatch.Identity)
	assert.IsType(t, (*computenicolegacy.Config)(nil), mismatch.Want)
	assert.Contains(t, err.Error(), "compute/nicolegacy.Config")
}

func writeServiceConfig(t *testing.T, data string) string {
	t.Helper()

	path := filepath.Join(t.TempDir(), "componentmanager.yaml")
	err := os.WriteFile(path, []byte(data), 0o600)
	require.NoError(t, err)
	return path
}

func requireDescriptor(
	t *testing.T,
	catalog cmcatalog.Catalog,
	componentType devicetypes.ComponentType,
	implementation string,
) cmcatalog.Descriptor {
	t.Helper()

	descriptor, ok := catalog.Get(cmcatalog.DescriptorIdentity{
		Type:           componentType,
		Implementation: implementation,
	})
	require.True(t, ok)
	return descriptor
}

func assertDescriptorCapabilities(
	t *testing.T,
	descriptor cmcatalog.Descriptor,
	capabilities ...capability.Capability,
) {
	t.Helper()

	expected, err := capability.CapabilitySet(capabilities).Normalize()
	require.NoError(t, err)
	assert.Equal(t, expected, descriptor.Capabilities)
}

func mockCapabilities() capability.CapabilitySet {
	return capability.CapabilitySet{
		capability.CapabilityBringUpControl,
		capability.CapabilityBringUpStatus,
		capability.CapabilityFirmwareControl,
		capability.CapabilityFirmwareStatus,
		capability.CapabilityInjectExpectation,
		capability.CapabilityPowerControl,
		capability.CapabilityPowerStatus,
	}
}
