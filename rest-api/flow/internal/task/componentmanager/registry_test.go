// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package componentmanager

import (
	"errors"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	"github.com/stretchr/testify/require"

	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestRegistryGetManager(t *testing.T) {
	t.Run("nil registry", func(t *testing.T) {
		var registry *Registry

		manager, err := registry.GetManager(devicetypes.ComponentTypeCompute)

		require.Nil(t, manager)
		require.Error(t, err)
		require.True(t, errors.Is(err, ErrRegistryNotConfigured))
	})

	t.Run("missing active manager", func(t *testing.T) {
		registry, err := NewRegistry(
			nil,
			cmconfig.Config{},
			providerapi.NewProviderRegistry(),
		)
		require.NoError(t, err)

		manager, err := registry.GetManager(devicetypes.ComponentTypeCompute)

		require.Nil(t, manager)
		require.Error(t, err)
		require.True(t, errors.Is(err, ErrManagerNotConfigured))

		var managerErr ManagerNotConfiguredError
		require.True(t, errors.As(err, &managerErr))
		require.Equal(t, devicetypes.ComponentTypeCompute, managerErr.ComponentType)
	})
}

func TestRegistryGetCapableManager(t *testing.T) {
	t.Run("nil registry", func(t *testing.T) {
		var registry *Registry

		manager, err := registry.GetCapableManager(
			devicetypes.ComponentTypeCompute,
			capability.CapabilityPowerControl,
		)

		require.Nil(t, manager)
		require.Error(t, err)
		require.True(t, errors.Is(err, ErrRegistryNotConfigured))
	})

	t.Run("missing active manager", func(t *testing.T) {
		registry, err := NewRegistry(
			nil,
			cmconfig.Config{},
			providerapi.NewProviderRegistry(),
		)
		require.NoError(t, err)

		manager, err := registry.GetCapableManager(
			devicetypes.ComponentTypeCompute,
			capability.CapabilityPowerControl,
		)

		require.Nil(t, manager)
		require.Error(t, err)
		require.True(t, errors.Is(err, ErrManagerNotConfigured))
	})

	t.Run("unknown capability", func(t *testing.T) {
		registry := &Registry{}

		manager, err := registry.GetCapableManager(
			devicetypes.ComponentTypeCompute,
			capability.Capability("PowerStatsu"),
		)

		require.Nil(t, manager)
		require.Error(t, err)
		require.True(t, errors.Is(err, ErrUnknownCapability))
	})

	t.Run("supported capability", func(t *testing.T) {
		registry := newRegistryWithCapabilities(
			t,
			capability.CapabilityPowerControl,
		)

		manager, err := registry.GetCapableManager(
			devicetypes.ComponentTypeCompute,
			capability.CapabilityPowerControl,
		)

		require.NoError(t, err)
		require.NotNil(t, manager)
	})

	t.Run("unsupported capability", func(t *testing.T) {
		registry := newRegistryWithCapabilities(
			t,
			capability.CapabilityPowerControl,
		)

		manager, err := registry.GetCapableManager(
			devicetypes.ComponentTypeCompute,
			capability.CapabilityFirmwareControl,
		)

		require.Nil(t, manager)
		require.Error(t, err)
		require.True(t, errors.Is(err, ErrUnsupportedCapability))

		var capabilityErr UnsupportedCapabilityError
		require.True(t, errors.As(err, &capabilityErr))
		require.Equal(t, devicetypes.ComponentTypeCompute, capabilityErr.ComponentType)
		require.Equal(t, "custom", capabilityErr.Implementation)
		require.Equal(t, capability.CapabilityFirmwareControl, capabilityErr.Capability)
		require.Equal(
			t,
			capability.CapabilitySet{capability.CapabilityPowerControl},
			capabilityErr.Available,
		)
	})
}

func TestRegistryCheckCapability(t *testing.T) {
	registry := newRegistryWithCapabilities(
		t,
		capability.CapabilityPowerControl,
	)

	err := registry.CheckCapability(
		devicetypes.ComponentTypeCompute,
		capability.CapabilityPowerControl,
	)
	require.NoError(t, err)

	err = registry.CheckCapability(
		devicetypes.ComponentTypeCompute,
		capability.CapabilityFirmwareControl,
	)
	require.Error(t, err)
	require.True(t, errors.Is(err, ErrUnsupportedCapability))
}

func TestRegistryGetDescriptor(t *testing.T) {
	registry, err := NewRegistry(
		[]FactorySpec{
			testFactorySpec(
				devicetypes.ComponentTypeCompute,
				"custom",
				managerFactory(devicetypes.ComponentTypeCompute, "custom"),
			),
		},
		cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute: "custom",
			},
		},
		providerapi.NewProviderRegistry(),
	)
	require.NoError(t, err)

	descriptor, err := registry.GetDescriptor(devicetypes.ComponentTypeCompute)

	require.NoError(t, err)
	require.Equal(t, devicetypes.ComponentTypeCompute, descriptor.Type)
	require.Equal(t, "custom", descriptor.Implementation)
}

func TestRegistryDescriptorResultsDoNotShareSliceStorage(t *testing.T) {
	descriptor := cmcatalog.Descriptor{
		DescriptorIdentity: cmcatalog.DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "custom",
		},
		RequiredProviders: []string{"provider"},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityPowerControl,
		},
	}
	registry, err := NewRegistry(
		[]FactorySpec{
			{
				Descriptor: descriptor,
				Factory: func(*providerapi.ProviderRegistry) (ComponentManager, error) {
					return testManager{descriptor: descriptor}, nil
				},
			},
		},
		cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute: "custom",
			},
		},
		providerapi.NewProviderRegistry(),
	)
	require.NoError(t, err)

	got, err := registry.GetDescriptor(devicetypes.ComponentTypeCompute)
	require.NoError(t, err)
	got.RequiredProviders[0] = "mutated"
	got.Capabilities[0] = capability.CapabilityFirmwareControl

	got, err = registry.GetDescriptor(devicetypes.ComponentTypeCompute)
	require.NoError(t, err)
	require.Equal(t, []string{"provider"}, got.RequiredProviders)
	require.Equal(
		t,
		capability.CapabilitySet{capability.CapabilityPowerControl},
		got.Capabilities,
	)

	descriptors, err := registry.Descriptors()
	require.NoError(t, err)
	descriptors[0].RequiredProviders[0] = "mutated"
	descriptors[0].Capabilities[0] = capability.CapabilityFirmwareControl

	descriptors, err = registry.Descriptors()
	require.NoError(t, err)
	require.Equal(t, []string{"provider"}, descriptors[0].RequiredProviders)
	require.Equal(
		t,
		capability.CapabilitySet{capability.CapabilityPowerControl},
		descriptors[0].Capabilities,
	)
}

func TestRegistryGetDescriptorErrors(t *testing.T) {
	t.Run("nil registry", func(t *testing.T) {
		var registry *Registry

		descriptor, err := registry.GetDescriptor(devicetypes.ComponentTypeCompute)

		require.Equal(t, cmcatalog.Descriptor{}, descriptor)
		require.Error(t, err)
		require.True(t, errors.Is(err, ErrRegistryNotConfigured))
	})

	t.Run("missing active manager", func(t *testing.T) {
		registry, err := NewRegistry(
			nil,
			cmconfig.Config{},
			providerapi.NewProviderRegistry(),
		)
		require.NoError(t, err)

		descriptor, err := registry.GetDescriptor(devicetypes.ComponentTypeCompute)

		require.Equal(t, cmcatalog.Descriptor{}, descriptor)
		require.Error(t, err)
		require.True(t, errors.Is(err, ErrManagerNotConfigured))
	})
}

func TestNewRegistryErrors(t *testing.T) {
	t.Run("factory not registered", func(t *testing.T) {
		_, err := NewRegistry(
			nil,
			cmconfig.Config{
				ComponentManagers: map[devicetypes.ComponentType]string{
					devicetypes.ComponentTypeCompute: "mock",
				},
			},
			providerapi.NewProviderRegistry(),
		)

		require.Error(t, err)
		require.True(t, errors.Is(err, ErrComponentManagerFactoryNotRegistered))

		var factoryErr ComponentManagerFactoryNotRegisteredError
		require.True(t, errors.As(err, &factoryErr))
		require.Equal(t, devicetypes.ComponentTypeCompute, factoryErr.ComponentType)
	})

	t.Run("factory not configured", func(t *testing.T) {
		_, err := NewRegistry(
			[]FactorySpec{
				{
					Descriptor: testDescriptor(
						devicetypes.ComponentTypeCompute,
						"custom",
					),
				},
			},
			cmconfig.Config{
				ComponentManagers: map[devicetypes.ComponentType]string{
					devicetypes.ComponentTypeCompute: "custom",
				},
			},
			providerapi.NewProviderRegistry(),
		)

		require.Error(t, err)
		require.True(t, errors.Is(err, ErrComponentManagerFactoryNotConfigured))

		var factoryErr ComponentManagerFactoryNotConfiguredError
		require.True(t, errors.As(err, &factoryErr))
		require.Equal(t, devicetypes.ComponentTypeCompute, factoryErr.ComponentType)
		require.Equal(t, "custom", factoryErr.Implementation)
	})

	t.Run("unknown implementation", func(t *testing.T) {
		_, err := NewRegistry(
			[]FactorySpec{
				testFactorySpec(
					devicetypes.ComponentTypeCompute,
					"known",
					managerFactory(devicetypes.ComponentTypeCompute, "known"),
				),
			},
			cmconfig.Config{
				ComponentManagers: map[devicetypes.ComponentType]string{
					devicetypes.ComponentTypeCompute: "missing",
				},
			},
			providerapi.NewProviderRegistry(),
		)

		require.Error(t, err)
		require.True(t, errors.Is(err, ErrUnknownComponentManagerImplementation))

		var implErr UnknownComponentManagerImplementationError
		require.True(t, errors.As(err, &implErr))
		require.Equal(t, devicetypes.ComponentTypeCompute, implErr.ComponentType)
		require.Equal(t, "missing", implErr.Implementation)
		require.ElementsMatch(t, []string{"known"}, implErr.Available)
	})

	t.Run("implementation registered for another type", func(t *testing.T) {
		_, err := NewRegistry(
			[]FactorySpec{
				testFactorySpec(
					devicetypes.ComponentTypeCompute,
					"nico",
					managerFactory(devicetypes.ComponentTypeCompute, "nico"),
				),
				testFactorySpec(
					devicetypes.ComponentTypeNVSwitch,
					"nvswitchmanager",
					managerFactory(devicetypes.ComponentTypeNVSwitch, "nvswitchmanager"),
				),
			},
			cmconfig.Config{
				ComponentManagers: map[devicetypes.ComponentType]string{
					devicetypes.ComponentTypeCompute: "nvswitchmanager",
				},
			},
			providerapi.NewProviderRegistry(),
		)

		require.Error(t, err)
		require.True(t, errors.Is(err, ErrUnknownComponentManagerImplementation))

		var implErr UnknownComponentManagerImplementationError
		require.True(t, errors.As(err, &implErr))
		require.Equal(t, devicetypes.ComponentTypeCompute, implErr.ComponentType)
		require.Equal(t, "nvswitchmanager", implErr.Implementation)
		require.Equal(t, []string{"nico"}, implErr.Available)
		require.Equal(t, []devicetypes.ComponentType{
			devicetypes.ComponentTypeNVSwitch,
		}, implErr.RegisteredFor)
	})

	t.Run("manager creation failed", func(t *testing.T) {
		rootErr := errors.New("boom")

		_, err := NewRegistry(
			[]FactorySpec{
				testFactorySpec(
					devicetypes.ComponentTypeCompute,
					"broken",
					func(*providerapi.ProviderRegistry) (ComponentManager, error) {
						return nil, rootErr
					},
				),
			},
			cmconfig.Config{
				ComponentManagers: map[devicetypes.ComponentType]string{
					devicetypes.ComponentTypeCompute: "broken",
				},
			},
			providerapi.NewProviderRegistry(),
		)

		require.Error(t, err)
		require.True(t, errors.Is(err, ErrManagerCreationFailed))
		require.True(t, errors.Is(err, rootErr))

		var creationErr ManagerCreationError
		require.True(t, errors.As(err, &creationErr))
		require.Equal(t, devicetypes.ComponentTypeCompute, creationErr.ComponentType)
		require.Equal(t, "broken", creationErr.Implementation)
	})

	t.Run("manager not created", func(t *testing.T) {
		_, err := NewRegistry(
			[]FactorySpec{
				testFactorySpec(
					devicetypes.ComponentTypeCompute,
					"nil-manager",
					func(*providerapi.ProviderRegistry) (ComponentManager, error) {
						return nil, nil
					},
				),
			},
			cmconfig.Config{
				ComponentManagers: map[devicetypes.ComponentType]string{
					devicetypes.ComponentTypeCompute: "nil-manager",
				},
			},
			providerapi.NewProviderRegistry(),
		)

		require.Error(t, err)
		require.True(t, errors.Is(err, ErrManagerNotCreated))

		var nilErr ManagerNotCreatedError
		require.True(t, errors.As(err, &nilErr))
		require.Equal(t, devicetypes.ComponentTypeCompute, nilErr.ComponentType)
		require.Equal(t, "nil-manager", nilErr.Implementation)
	})

	t.Run("manager descriptor mismatch", func(t *testing.T) {
		_, err := NewRegistry(
			[]FactorySpec{
				testFactorySpec(
					devicetypes.ComponentTypeCompute,
					"wrong-type",
					managerFactory(devicetypes.ComponentTypeNVSwitch, "wrong-type"),
				),
			},
			cmconfig.Config{
				ComponentManagers: map[devicetypes.ComponentType]string{
					devicetypes.ComponentTypeCompute: "wrong-type",
				},
			},
			providerapi.NewProviderRegistry(),
		)

		require.Error(t, err)
		require.True(t, errors.Is(err, ErrManagerDescriptorMismatch))

		var mismatchErr ManagerDescriptorMismatchError
		require.True(t, errors.As(err, &mismatchErr))
		require.Equal(t, devicetypes.ComponentTypeCompute, mismatchErr.Expected.Type)
		require.Equal(t, "wrong-type", mismatchErr.Expected.Implementation)
		require.Equal(t, devicetypes.ComponentTypeNVSwitch, mismatchErr.Actual.Type)
		require.Equal(t, "wrong-type", mismatchErr.Actual.Implementation)
	})
}

func TestCreateManagerRejectsDescriptorMismatch(t *testing.T) {
	tests := []struct {
		name       string
		expected   cmcatalog.Descriptor
		factory    ManagerFactory
		wantActual cmcatalog.Descriptor
	}{
		{
			name: "type mismatch",
			expected: testDescriptor(
				devicetypes.ComponentTypeCompute,
				"custom",
			),
			factory: managerFactory(
				devicetypes.ComponentTypeNVSwitch,
				"custom",
			),
			wantActual: testDescriptor(
				devicetypes.ComponentTypeNVSwitch,
				"custom",
			),
		},
		{
			name: "implementation mismatch",
			expected: testDescriptor(
				devicetypes.ComponentTypeCompute,
				"custom",
			),
			factory: managerFactory(
				devicetypes.ComponentTypeCompute,
				"other",
			),
			wantActual: testDescriptor(
				devicetypes.ComponentTypeCompute,
				"other",
			),
		},
		{
			name: "required providers mismatch",
			expected: testDescriptor(
				devicetypes.ComponentTypeCompute,
				"custom",
				"alpha",
			),
			factory: managerFactory(
				devicetypes.ComponentTypeCompute,
				"custom",
				"beta",
			),
			wantActual: testDescriptor(
				devicetypes.ComponentTypeCompute,
				"custom",
				"beta",
			),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			expected, err := tt.expected.Normalize()
			require.NoError(t, err)
			wantActual, err := tt.wantActual.Normalize()
			require.NoError(t, err)

			manager, err := createManager(
				expected,
				tt.factory,
				providerapi.NewProviderRegistry(),
			)

			require.Nil(t, manager)
			require.Error(t, err)
			require.True(t, errors.Is(err, ErrManagerDescriptorMismatch))

			var mismatchErr ManagerDescriptorMismatchError
			require.True(t, errors.As(err, &mismatchErr))
			require.Equal(t, expected, mismatchErr.Expected)
			require.Equal(t, wantActual, mismatchErr.Actual)
		})
	}
}

func TestNewRegistryReturnsNilWhenManagerValidationFails(t *testing.T) {
	registry, err := NewRegistry(
		[]FactorySpec{
			testFactorySpec(
				devicetypes.ComponentTypeCompute,
				"compute",
				managerFactory(devicetypes.ComponentTypeCompute, "compute"),
			),
			testFactorySpec(
				devicetypes.ComponentTypeNVSwitch,
				"wrong-type",
				managerFactory(devicetypes.ComponentTypePowerShelf, "wrong-type"),
			),
		},
		cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute:  "compute",
				devicetypes.ComponentTypeNVSwitch: "wrong-type",
			},
		},
		providerapi.NewProviderRegistry(),
	)

	require.Nil(t, registry)
	require.Error(t, err)
	require.True(t, errors.Is(err, ErrManagerDescriptorMismatch))
}

func TestRegistryFindManager(t *testing.T) {
	t.Run("nil registry", func(t *testing.T) {
		var registry *Registry

		manager := registry.FindManager(devicetypes.ComponentTypeCompute)

		require.Nil(t, manager)
	})

	t.Run("missing active manager", func(t *testing.T) {
		registry, err := NewRegistry(
			nil,
			cmconfig.Config{},
			providerapi.NewProviderRegistry(),
		)
		require.NoError(t, err)

		manager := registry.FindManager(devicetypes.ComponentTypeCompute)

		require.Nil(t, manager)
	})
}

func TestRegistryComponentTypes(t *testing.T) {
	t.Run("nil registry", func(t *testing.T) {
		var registry *Registry

		componentTypes := registry.ComponentTypes()

		require.Nil(t, componentTypes)
	})

	registry, err := NewRegistry(
		[]FactorySpec{
			testFactorySpec(
				devicetypes.ComponentTypeNVSwitch,
				"switch",
				managerFactory(devicetypes.ComponentTypeNVSwitch, "switch"),
			),
			testFactorySpec(
				devicetypes.ComponentTypeCompute,
				"compute",
				managerFactory(devicetypes.ComponentTypeCompute, "compute"),
			),
		},
		cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeNVSwitch: "switch",
				devicetypes.ComponentTypeCompute:  "compute",
			},
		},
		providerapi.NewProviderRegistry(),
	)
	require.NoError(t, err)

	componentTypes := registry.ComponentTypes()

	require.Equal(t, []devicetypes.ComponentType{
		devicetypes.ComponentTypeCompute,
		devicetypes.ComponentTypeNVSwitch,
	}, componentTypes)

	componentTypes[0] = devicetypes.ComponentTypePowerShelf
	require.Equal(t, []devicetypes.ComponentType{
		devicetypes.ComponentTypeCompute,
		devicetypes.ComponentTypeNVSwitch,
	}, registry.ComponentTypes())
}

func TestRegistryDescriptors(t *testing.T) {
	t.Run("nil registry", func(t *testing.T) {
		var registry *Registry

		descriptors, err := registry.Descriptors()

		require.Nil(t, descriptors)
		require.Error(t, err)
		require.True(t, errors.Is(err, ErrRegistryNotConfigured))
	})

	registry, err := NewRegistry(
		[]FactorySpec{
			testFactorySpec(
				devicetypes.ComponentTypeNVSwitch,
				"switch",
				managerFactory(devicetypes.ComponentTypeNVSwitch, "switch"),
			),
			testFactorySpec(
				devicetypes.ComponentTypeCompute,
				"compute",
				managerFactory(devicetypes.ComponentTypeCompute, "compute"),
			),
		},
		cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeNVSwitch: "switch",
				devicetypes.ComponentTypeCompute:  "compute",
			},
		},
		providerapi.NewProviderRegistry(),
	)
	require.NoError(t, err)

	descriptors, err := registry.Descriptors()

	require.NoError(t, err)
	require.Equal(t, []cmcatalog.Descriptor{
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "compute",
			},
		},
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypeNVSwitch,
				Implementation: "switch",
			},
		},
	}, descriptors)
}

func TestRegistryComponentManagers(t *testing.T) {
	t.Run("nil registry", func(t *testing.T) {
		var registry *Registry

		managers := registry.ComponentManagers()

		require.Nil(t, managers)
	})

	registry, err := NewRegistry(
		[]FactorySpec{
			testFactorySpec(
				devicetypes.ComponentTypeNVSwitch,
				"switch",
				managerFactory(devicetypes.ComponentTypeNVSwitch, "switch"),
			),
			testFactorySpec(
				devicetypes.ComponentTypeCompute,
				"compute",
				managerFactory(devicetypes.ComponentTypeCompute, "compute"),
			),
		},
		cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeNVSwitch: "switch",
				devicetypes.ComponentTypeCompute:  "compute",
			},
		},
		providerapi.NewProviderRegistry(),
	)
	require.NoError(t, err)

	managers := registry.ComponentManagers()

	require.Len(t, managers, 2)
	descriptors := make([]cmcatalog.Descriptor, 0, len(managers))
	for _, manager := range managers {
		descriptors = append(descriptors, manager.Descriptor())
	}
	require.Equal(t, []cmcatalog.Descriptor{
		testDescriptor(devicetypes.ComponentTypeCompute, "compute"),
		testDescriptor(devicetypes.ComponentTypeNVSwitch, "switch"),
	}, descriptors)
}
