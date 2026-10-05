// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"gopkg.in/yaml.v3"

	cmcatalog "github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/componentmanager/providers/nico"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

type customProviderConfig struct {
	name string
}

func (c customProviderConfig) Name() string {
	return c.name
}

func (c customProviderConfig) NewProvider(context.Context) (providerapi.Provider, error) {
	return nil, nil
}

type customProviderConfigDecoder struct {
	name string
}

func (d customProviderConfigDecoder) Name() string {
	return d.name
}

func (d customProviderConfigDecoder) DefaultConfig() providerapi.ProviderConfig {
	return customProviderConfig{name: d.name}
}

func (d customProviderConfigDecoder) DecodeYAML(raw yaml.Node) (providerapi.ProviderConfig, error) {
	return d.DefaultConfig(), nil
}

type customManagerConfig struct {
	identity cmcatalog.DescriptorIdentity
	value    string
}

func (c customManagerConfig) Validate(expectedIdentity cmcatalog.DescriptorIdentity) error {
	if c.identity != expectedIdentity {
		return ManagerConfigIdentityMismatchError{
			Expected: expectedIdentity,
			Actual:   c.identity,
		}
	}
	return nil
}

type customManagerConfigDecoder struct {
	identity       cmcatalog.DescriptorIdentity
	configIdentity cmcatalog.DescriptorIdentity
	defaultValue   string
}

func (d customManagerConfigDecoder) Identity() cmcatalog.DescriptorIdentity {
	return d.identity
}

func (d customManagerConfigDecoder) DefaultConfig() ManagerConfig {
	return customManagerConfig{
		identity: d.managerConfigIdentity(),
		value:    d.defaultValue,
	}
}

func (d customManagerConfigDecoder) managerConfigIdentity() cmcatalog.DescriptorIdentity {
	if d.configIdentity != (cmcatalog.DescriptorIdentity{}) {
		return d.configIdentity
	}
	return d.identity
}

func (d customManagerConfigDecoder) DecodeYAML(raw yaml.Node) (ManagerConfig, error) {
	config := d.DefaultConfig().(customManagerConfig)
	var parsed struct {
		Value string `yaml:"value"`
	}
	if err := DecodeYAMLStrict(raw, &parsed); err != nil {
		return nil, InvalidManagerConfigError{
			Identity: d.identity,
			Err:      err,
		}
	}
	if parsed.Value != "" {
		config.value = parsed.Value
	}
	return config, nil
}

func TestParseConfigWithExplicitProviders(t *testing.T) {
	config, err := parseConfigWithBuiltins(t, `
component_managers:
  compute: nico
  nvswitch: nico
  powershelf: nico
providers:
  nico:
    timeout: 45s
`)
	require.NoError(t, err)

	assert.Equal(t, nico.ProviderName, config.ComponentManagers[devicetypes.ComponentTypeCompute])
	assert.Equal(t, nico.ProviderName, config.ComponentManagers[devicetypes.ComponentTypeNVSwitch])
	assert.Equal(t, nico.ProviderName, config.ComponentManagers[devicetypes.ComponentTypePowerShelf])

	nicoConfig, ok := config.ProviderConfigs[nico.ProviderName].(*nico.Config)
	require.True(t, ok)
	assert.Equal(t, 45*time.Second, nicoConfig.Timeout)
}

func TestParseConfigDerivesProviders(t *testing.T) {
	tests := []struct {
		name        string
		configYAML  string
		wantEnabled []string
	}{
		{
			name: "mock managers do not need providers",
			configYAML: `
component_managers:
  compute: mock
  nvswitch: mock
  powershelf: mock
`,
			wantEnabled: nil,
		},
		{
			name: "nico",
			configYAML: `
component_managers:
  compute: nico
`,
			wantEnabled: []string{nico.ProviderName},
		},
		{
			name: "deduplicates providers",
			configYAML: `
component_managers:
  compute: nico
  nvswitch: nico
  powershelf: nico
`,
			wantEnabled: []string{nico.ProviderName},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			config, err := parseConfigWithBuiltins(t, tc.configYAML)
			require.NoError(t, err)
			assert.ElementsMatch(t, tc.wantEnabled, providerConfigNames(config))
		})
	}
}

func TestParseConfigCompletesMissingExplicitProviders(t *testing.T) {
	config, err := parseConfigWithBuiltins(t, `
component_managers:
  compute: nico
  powershelf: nico
providers:
  nico:
    timeout: 20s
`)
	require.NoError(t, err)

	assert.True(t, config.HasProvider(nico.ProviderName))

	nicoConfig, ok := config.ProviderConfigs[nico.ProviderName].(*nico.Config)
	require.True(t, ok)
	assert.Equal(t, 20*time.Second, nicoConfig.Timeout)
}

func TestParseConfigCompletesEmptyProviders(t *testing.T) {
	config, err := parseConfigWithBuiltins(t, `
component_managers:
  compute: nico
providers: {}
`)
	require.NoError(t, err)

	assert.True(t, config.HasProvider(nico.ProviderName))

	err = config.Validate(serviceProviderConfigDecoderRegistry(t), testCatalog(t), nil)
	require.NoError(t, err)
}

func TestParseConfigErrors(t *testing.T) {
	tests := []struct {
		name       string
		configYAML string
		wantErr    error
		checkErr   func(*testing.T, error)
	}{
		{
			name: "unknown provider",
			configYAML: `
component_managers:
  compute: mock
providers:
  madeup: {}
`,
			wantErr: providerapi.ErrUnknownProvider,
			checkErr: func(t *testing.T, err error) {
				t.Helper()
				var providerErr providerapi.UnknownProviderError
				require.True(t, errors.As(err, &providerErr))
				assert.Equal(t, "madeup", providerErr.Name)
			},
		},
		{
			name: "unknown component type",
			configYAML: `
component_managers:
  madeup: mock
`,
			wantErr: ErrUnknownComponentType,
			checkErr: func(t *testing.T, err error) {
				t.Helper()
				var ctErr UnknownComponentTypeError
				require.True(t, errors.As(err, &ctErr))
				assert.Equal(t, "madeup", ctErr.Name)
			},
		},
		{
			name: "empty implementation name",
			configYAML: `
component_managers:
  compute: " "
`,
			wantErr: ErrComponentManagerImplementationNameEmpty,
			checkErr: func(t *testing.T, err error) {
				t.Helper()
				var nameErr ComponentManagerImplementationNameEmptyError
				require.True(t, errors.As(err, &nameErr))
				assert.Equal(t, devicetypes.ComponentTypeCompute, nameErr.ComponentType)
			},
		},
		{
			name: "duplicate provider after trimming name",
			configYAML: `
component_managers:
  compute: mock
providers:
  nico:
    timeout: 30s
  " nico ":
    timeout: 45s
`,
			wantErr: ErrDuplicateProviderConfig,
			checkErr: func(t *testing.T, err error) {
				t.Helper()
				var duplicateErr DuplicateProviderConfigError
				require.True(t, errors.As(err, &duplicateErr))
				assert.Equal(t, nico.ProviderName, duplicateErr.Name)
			},
		},
		{
			name: "invalid nico timeout",
			configYAML: `
component_managers:
  compute: mock
providers:
  nico:
    timeout: nope
`,
			wantErr: providerapi.ErrInvalidProviderConfigField,
			checkErr: func(t *testing.T, err error) {
				t.Helper()
				assertInvalidProviderConfigField(t, err, nico.ProviderName, "timeout")
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			_, err := parseConfigWithBuiltins(t, tc.configYAML)
			require.Error(t, err)
			assert.True(t, errors.Is(err, tc.wantErr))
			if tc.checkErr != nil {
				tc.checkErr(t, err)
			}
		})
	}
}

func TestParseConfigAllowsCustomProviderDecoderRegistry(t *testing.T) {
	registry := providerapi.NewProviderConfigDecoderRegistry()
	require.NoError(t, registry.Register(customProviderConfigDecoder{name: "custom"}))

	config, err := ParseConfig([]byte(`
component_managers:
  compute: mock
providers:
  custom: {}
`), registry, testCatalog(t), nil)
	require.NoError(t, err)

	assert.True(t, config.HasProvider("custom"))
	assert.Equal(t, customProviderConfig{name: "custom"}, config.ProviderConfigs["custom"])
}

func TestParseConfigDerivesProviderForDifferentImplementationName(t *testing.T) {
	registry := serviceProviderConfigDecoderRegistry(t)
	require.NoError(t, registry.Register(customProviderConfigDecoder{name: "custom"}))

	config, err := ParseConfig([]byte(`
component_managers:
  compute: custom-manager
`), registry, testCatalog(t), nil)
	require.NoError(t, err)

	assert.True(t, config.HasProvider("custom"))
}

func TestParseConfigDerivesMultipleProvidersForManager(t *testing.T) {
	registry := serviceProviderConfigDecoderRegistry(t)
	require.NoError(t, registry.Register(customProviderConfigDecoder{name: "custom"}))

	config, err := ParseConfig([]byte(`
component_managers:
  compute: multi-provider
`), registry, testCatalog(t), nil)
	require.NoError(t, err)

	assert.True(t, config.HasProvider("custom"))
	assert.True(t, config.HasProvider(nico.ProviderName))
}

func TestParseConfigRejectsManagerConfigFieldInProviderConfig(t *testing.T) {
	_, err := ParseConfig([]byte(`
component_managers:
  compute: nico
providers:
  nico:
    compute_power_delay: 0s
`), serviceProviderConfigDecoderRegistry(t), testCatalog(t), nil)

	require.Error(t, err)
	assert.True(t, errors.Is(err, providerapi.ErrInvalidProviderConfig))
}

func TestParseConfigWithExplicitManagerConfigs(t *testing.T) {
	identity := testManagerConfigIdentity()
	managerDecoders := testManagerConfigDecoderRegistry(t, customManagerConfigDecoder{
		identity:     identity,
		defaultValue: "default",
	})

	config, err := ParseConfig([]byte(`
component_managers:
  compute: configurable-manager
manager_configs:
  compute:
    configurable-manager:
      value: configured
`), serviceProviderConfigDecoderRegistry(t), testCatalog(t), managerDecoders)
	require.NoError(t, err)

	assert.Equal(
		t,
		customManagerConfig{
			identity: identity,
			value:    "configured",
		},
		config.ManagerConfigs[identity],
	)
}

func TestParseConfigRejectsManagerConfigUnknownImplementation(t *testing.T) {
	const unknownImpl = "unknown-impl"
	_, err := ParseConfig([]byte(`
component_managers:
  compute: unknown-impl
manager_configs:
  compute:
    unknown-impl:
      value: configured
`), serviceProviderConfigDecoderRegistry(t), testCatalog(t), NewManagerConfigDecoderRegistry())

	require.Error(t, err)
	assert.True(t, errors.Is(err, cmcatalog.ErrUnknownComponentManagerImplementation))

	var implErr cmcatalog.UnknownComponentManagerImplementationError
	require.True(t, errors.As(err, &implErr))
	assert.Equal(t, devicetypes.ComponentTypeCompute, implErr.ComponentType)
	assert.Equal(t, unknownImpl, implErr.Implementation)
	assert.Contains(t, implErr.Available, nico.ProviderName)
}

func TestParseConfigCompletesDefaultManagerConfigs(t *testing.T) {
	identity := testManagerConfigIdentity()
	managerDecoders := testManagerConfigDecoderRegistry(t, customManagerConfigDecoder{
		identity:     identity,
		defaultValue: "default",
	})

	config, err := New(
		map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypeCompute: "configurable-manager",
		},
		serviceProviderConfigDecoderRegistry(t),
		testCatalog(t),
		managerDecoders,
	)
	require.NoError(t, err)

	assert.Equal(
		t,
		customManagerConfig{
			identity: identity,
			value:    "default",
		},
		config.ManagerConfigs[identity],
	)
}

func TestNewConfigRejectsManagerConfigIdentityMismatch(t *testing.T) {
	identity := testManagerConfigIdentity()
	actualIdentity := cmcatalog.DescriptorIdentity{
		Type:           devicetypes.ComponentTypeCompute,
		Implementation: "custom-manager",
	}
	managerDecoders := testManagerConfigDecoderRegistry(t, customManagerConfigDecoder{
		identity:       identity,
		configIdentity: actualIdentity,
		defaultValue:   "default",
	})

	_, err := New(
		map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypeCompute: "configurable-manager",
		},
		serviceProviderConfigDecoderRegistry(t),
		testCatalog(t),
		managerDecoders,
	)

	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrManagerConfigIdentityMismatch))

	var mismatch ManagerConfigIdentityMismatchError
	require.True(t, errors.As(err, &mismatch))
	assert.Equal(t, identity, mismatch.Expected)
	assert.Equal(t, actualIdentity, mismatch.Actual)
}

func TestValidateRejectsUnnormalizedManagerConfigIdentity(t *testing.T) {
	identity := testManagerConfigIdentity()
	rawIdentity := cmcatalog.DescriptorIdentity{
		Type:           identity.Type,
		Implementation: " configurable-manager ",
	}
	config := Config{
		ComponentManagers: map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypeCompute: "configurable-manager",
		},
		ManagerConfigs: map[cmcatalog.DescriptorIdentity]ManagerConfig{
			rawIdentity: customManagerConfig{identity: identity},
		},
		ProviderConfigs: map[string]providerapi.ProviderConfig{},
	}

	err := config.Validate(
		serviceProviderConfigDecoderRegistry(t),
		testCatalog(t),
		testManagerConfigDecoderRegistry(t, customManagerConfigDecoder{identity: identity}),
	)

	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrManagerConfigIdentityMismatch))

	var mismatch ManagerConfigIdentityMismatchError
	require.True(t, errors.As(err, &mismatch))
	assert.Equal(t, identity, mismatch.Expected)
	assert.Equal(t, rawIdentity, mismatch.Actual)
}

func TestParseConfigRequiresDecoderRegistry(t *testing.T) {
	_, err := ParseConfig([]byte(`component_managers: {}`), nil, cmcatalog.Catalog{}, nil)
	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrProviderConfigDecoderRegistryRequired))
}

func TestParseConfigRequiresManagerDecoderRegistryForManagerConfigs(t *testing.T) {
	_, err := ParseConfig([]byte(`
component_managers:
  compute: configurable-manager
manager_configs:
  compute:
    configurable-manager:
      value: configured
`), serviceProviderConfigDecoderRegistry(t), testCatalog(t), nil)
	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrManagerConfigDecoderRegistryRequired))
}

func TestNewConfigRequiresDecoderRegistry(t *testing.T) {
	_, err := New(map[devicetypes.ComponentType]string{}, nil, cmcatalog.Catalog{}, nil)
	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrProviderConfigDecoderRegistryRequired))
}

func TestValidateRequiresConfig(t *testing.T) {
	var config *Config

	err := config.Validate(serviceProviderConfigDecoderRegistry(t), cmcatalog.Catalog{}, nil)

	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrConfigNotConfigured))
}

func TestValidateReportsRequiredProviderManagerIdentity(t *testing.T) {
	config := Config{
		ComponentManagers: map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypeCompute: nico.ProviderName,
		},
		ProviderConfigs: map[string]providerapi.ProviderConfig{},
	}

	err := config.Validate(
		serviceProviderConfigDecoderRegistry(t),
		testCatalog(t),
		nil,
	)

	require.Error(t, err)
	assert.True(t, errors.Is(err, providerapi.ErrProviderNotConfigured))

	var requiredErr RequiredProviderNotConfiguredError
	require.True(t, errors.As(err, &requiredErr))
	assert.Equal(t, nico.ProviderName, requiredErr.Provider)
	assert.Equal(t, devicetypes.ComponentTypeCompute, requiredErr.ComponentType)
	assert.Equal(t, nico.ProviderName, requiredErr.Implementation)
}

func TestCompleteProviderConfigsReportsRequiredProviderDecoderManagerIdentity(t *testing.T) {
	config := Config{
		ComponentManagers: map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypeCompute: "custom-manager",
		},
		ProviderConfigs: map[string]providerapi.ProviderConfig{},
	}

	err := config.completeProviderConfigs(
		providerapi.NewProviderConfigDecoderRegistry(),
		testCatalog(t),
	)

	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrProviderConfigDecoderNotRegistered))

	var decoderErr ProviderConfigDecoderNotRegisteredError
	require.True(t, errors.As(err, &decoderErr))
	assert.Equal(t, "custom", decoderErr.Name)
	assert.Equal(t, devicetypes.ComponentTypeCompute, decoderErr.ComponentType)
	assert.Equal(t, "custom-manager", decoderErr.Implementation)
}

func assertInvalidProviderConfigField(
	t *testing.T,
	err error,
	provider string,
	field string,
) {
	t.Helper()

	var fieldErr providerapi.InvalidProviderConfigFieldError
	require.True(t, errors.As(err, &fieldErr))
	assert.Equal(t, provider, fieldErr.Provider)
	assert.Equal(t, field, fieldErr.Field)
}

func TestHasProviderUsesProviderConfigs(t *testing.T) {
	config := Config{
		ProviderConfigs: map[string]providerapi.ProviderConfig{
			nico.ProviderName: &nico.Config{},
		},
	}

	assert.True(t, config.HasProvider(nico.ProviderName))
	assert.False(t, config.HasProvider("unregistered-provider"))
}

func TestNewConfigDerivesDefaultProviderConfigs(t *testing.T) {
	config, err := New(
		map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypeCompute: nico.ProviderName,
		},
		serviceProviderConfigDecoderRegistry(t),
		testCatalog(t),
		nil,
	)
	require.NoError(t, err)

	assert.True(t, config.HasProvider(nico.ProviderName))
	nicoConfig, ok := config.ProviderConfigs[nico.ProviderName].(*nico.Config)
	require.True(t, ok)
	assert.Equal(t, nico.DefaultTimeout, nicoConfig.Timeout)
}

func TestLoadConfig(t *testing.T) {
	path := filepath.Join(t.TempDir(), "componentmanager.yaml")
	err := os.WriteFile(path, []byte(`
component_managers:
  compute: nico
`), 0o600)
	require.NoError(t, err)

	config, err := LoadConfig(
		path,
		serviceProviderConfigDecoderRegistry(t),
		testCatalog(t),
		nil,
	)
	require.NoError(t, err)
	assert.True(t, config.HasProvider(nico.ProviderName))
}

func providerConfigNames(config Config) []string {
	names := make([]string, 0, len(config.ProviderConfigs))
	for name := range config.ProviderConfigs {
		names = append(names, name)
	}
	return names
}

func parseConfigWithBuiltins(t *testing.T, data string) (Config, error) {
	t.Helper()
	return ParseConfig(
		[]byte(data),
		serviceProviderConfigDecoderRegistry(t),
		testCatalog(t),
		nil,
	)
}

func testCatalog(t *testing.T) cmcatalog.Catalog {
	t.Helper()

	catalog, err := cmcatalog.New([]cmcatalog.Descriptor{
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "mock",
			},
		},
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypeNVSwitch,
				Implementation: "mock",
			},
		},
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypePowerShelf,
				Implementation: "mock",
			},
		},
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: nico.ProviderName,
			},
			RequiredProviders: []string{nico.ProviderName},
		},
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypeNVSwitch,
				Implementation: nico.ProviderName,
			},
			RequiredProviders: []string{nico.ProviderName},
		},
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypePowerShelf,
				Implementation: nico.ProviderName,
			},
			RequiredProviders: []string{nico.ProviderName},
		},
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "custom-manager",
			},
			RequiredProviders: []string{"custom"},
		},
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "configurable-manager",
			},
		},
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "multi-provider",
			},
			RequiredProviders: []string{
				"custom",
				nico.ProviderName,
			},
		},
	})
	require.NoError(t, err)

	return catalog
}

func serviceProviderConfigDecoderRegistry(t *testing.T) *providerapi.ProviderConfigDecoderRegistry {
	t.Helper()
	registry := providerapi.NewProviderConfigDecoderRegistry()
	require.NoError(t, registry.Register(nico.ConfigDecoder{}))
	return registry
}

func testManagerConfigIdentity() cmcatalog.DescriptorIdentity {
	return cmcatalog.DescriptorIdentity{
		Type:           devicetypes.ComponentTypeCompute,
		Implementation: "configurable-manager",
	}
}

func testManagerConfigDecoderRegistry(
	t *testing.T,
	decoders ...ManagerConfigDecoder,
) *ManagerConfigDecoderRegistry {
	t.Helper()
	registry := NewManagerConfigDecoderRegistry()
	for _, decoder := range decoders {
		require.NoError(t, registry.Register(decoder))
	}
	return registry
}
