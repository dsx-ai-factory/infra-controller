// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package catalog

import (
	"errors"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestDescriptorNormalize(t *testing.T) {
	descriptor, err := Descriptor{
		DescriptorIdentity: DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: " custom ",
		},
		RequiredProviders: []string{" beta ", "alpha", "beta"},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityPowerControl,
			" FirmwareControl ",
			capability.CapabilityPowerControl,
		},
	}.Normalize()

	require.NoError(t, err)
	require.Equal(t, Descriptor{
		DescriptorIdentity: DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "custom",
		},
		RequiredProviders: []string{"alpha", "beta"},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityFirmwareControl,
			capability.CapabilityPowerControl,
		},
	}, descriptor)
}

func TestDescriptorEqual(t *testing.T) {
	descriptor := Descriptor{
		DescriptorIdentity: DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "custom",
		},
		RequiredProviders: []string{"alpha", "beta"},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityFirmwareControl,
			capability.CapabilityPowerControl,
		},
	}

	require.True(t, descriptor.Equal(Descriptor{
		DescriptorIdentity: DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "custom",
		},
		RequiredProviders: []string{"alpha", "beta"},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityFirmwareControl,
			capability.CapabilityPowerControl,
		},
	}))
	require.False(t, descriptor.Equal(Descriptor{
		DescriptorIdentity: DescriptorIdentity{
			Type:           devicetypes.ComponentTypeNVSwitch,
			Implementation: "custom",
		},
		RequiredProviders: []string{"alpha", "beta"},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityFirmwareControl,
			capability.CapabilityPowerControl,
		},
	}))
	require.False(t, descriptor.Equal(Descriptor{
		DescriptorIdentity: DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "other",
		},
		RequiredProviders: []string{"alpha", "beta"},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityFirmwareControl,
			capability.CapabilityPowerControl,
		},
	}))
	require.False(t, descriptor.Equal(Descriptor{
		DescriptorIdentity: DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "custom",
		},
		RequiredProviders: []string{"alpha"},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityFirmwareControl,
			capability.CapabilityPowerControl,
		},
	}))
	require.False(t, descriptor.Equal(Descriptor{
		DescriptorIdentity: DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "custom",
		},
		RequiredProviders: []string{"alpha", "beta"},
		Capabilities:      capability.CapabilitySet{capability.CapabilityPowerControl},
	}))
}

func TestDescriptorNormalizeRejectsInvalidDescriptor(t *testing.T) {
	tests := []struct {
		name       string
		descriptor Descriptor
		wantErr    error
	}{
		{
			name: "unknown component type",
			descriptor: Descriptor{
				DescriptorIdentity: DescriptorIdentity{
					Type:           devicetypes.ComponentTypeUnknown,
					Implementation: "custom",
				},
			},
			wantErr: ErrUnknownComponentType,
		},
		{
			name: "empty implementation",
			descriptor: Descriptor{
				DescriptorIdentity: DescriptorIdentity{
					Type:           devicetypes.ComponentTypeCompute,
					Implementation: " ",
				},
			},
			wantErr: ErrComponentManagerImplementationNameEmpty,
		},
		{
			name: "empty required provider",
			descriptor: Descriptor{
				DescriptorIdentity: DescriptorIdentity{
					Type:           devicetypes.ComponentTypeCompute,
					Implementation: "custom",
				},
				RequiredProviders: []string{"nico", " "},
			},
			wantErr: providerapi.ErrProviderNameEmpty,
		},
		{
			name: "empty capability",
			descriptor: Descriptor{
				DescriptorIdentity: DescriptorIdentity{
					Type:           devicetypes.ComponentTypeCompute,
					Implementation: "custom",
				},
				Capabilities: capability.CapabilitySet{" "},
			},
			wantErr: capability.ErrNameEmpty,
		},
		{
			name: "unknown capability",
			descriptor: Descriptor{
				DescriptorIdentity: DescriptorIdentity{
					Type:           devicetypes.ComponentTypeCompute,
					Implementation: "custom",
				},
				Capabilities: capability.CapabilitySet{"PowerStatsu"},
			},
			wantErr: capability.ErrUnknown,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := tt.descriptor.Normalize()

			require.Error(t, err)
			require.True(t, errors.Is(err, tt.wantErr))
		})
	}
}

func TestNewIndexesNormalizedDescriptors(t *testing.T) {
	catalog, err := New([]Descriptor{
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: " custom ",
			},
			RequiredProviders: []string{" beta ", "alpha", "beta"},
		},
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "builtin",
			},
		},
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypePowerShelf,
				Implementation: "psm",
			},
		},
	})
	require.NoError(t, err)

	descriptor, ok := catalog.Get(DescriptorIdentity{
		Type:           devicetypes.ComponentTypeCompute,
		Implementation: "custom",
	})
	require.True(t, ok)
	require.Equal(t, Descriptor{
		DescriptorIdentity: DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "custom",
		},
		RequiredProviders: []string{"alpha", "beta"},
	}, descriptor)

	_, ok = catalog.Get(DescriptorIdentity{
		Type:           devicetypes.ComponentTypeCompute,
		Implementation: "missing",
	})
	require.False(t, ok)
	_, ok = catalog.Get(DescriptorIdentity{
		Type:           devicetypes.ComponentTypeNVSwitch,
		Implementation: "custom",
	})
	require.False(t, ok)

	require.Equal(
		t,
		[]string{"builtin", "custom"},
		catalog.Implementations(devicetypes.ComponentTypeCompute),
	)
	require.Empty(t, catalog.Implementations(devicetypes.ComponentTypeNVSwitch))
	require.Equal(
		t,
		map[devicetypes.ComponentType][]string{
			devicetypes.ComponentTypeCompute:    {"builtin", "custom"},
			devicetypes.ComponentTypePowerShelf: {"psm"},
		},
		catalog.ListImplementations(),
	)
}

func TestGetReturnsDescriptorCopy(t *testing.T) {
	catalog, err := New([]Descriptor{
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "custom",
			},
			RequiredProviders: []string{"alpha", "beta"},
			Capabilities: capability.CapabilitySet{
				capability.CapabilityFirmwareControl,
				capability.CapabilityPowerControl,
			},
		},
	})
	require.NoError(t, err)

	descriptor, ok := catalog.Get(DescriptorIdentity{
		Type:           devicetypes.ComponentTypeCompute,
		Implementation: "custom",
	})
	require.True(t, ok)
	descriptor.RequiredProviders = append(descriptor.RequiredProviders[:1], "mutated")
	descriptor.Capabilities = append(descriptor.Capabilities[:1], "Mutated")

	descriptor, ok = catalog.Get(DescriptorIdentity{
		Type:           devicetypes.ComponentTypeCompute,
		Implementation: "custom",
	})
	require.True(t, ok)
	require.Equal(t, []string{"alpha", "beta"}, descriptor.RequiredProviders)
	require.Equal(
		t,
		capability.CapabilitySet{capability.CapabilityFirmwareControl, capability.CapabilityPowerControl},
		descriptor.Capabilities,
	)

	descriptor.RequiredProviders[0] = "mutated"
	descriptor.Capabilities[0] = "Mutated"

	descriptor, ok = catalog.Get(DescriptorIdentity{
		Type:           devicetypes.ComponentTypeCompute,
		Implementation: "custom",
	})
	require.True(t, ok)
	require.Equal(t, []string{"alpha", "beta"}, descriptor.RequiredProviders)
	require.Equal(
		t,
		capability.CapabilitySet{capability.CapabilityFirmwareControl, capability.CapabilityPowerControl},
		descriptor.Capabilities,
	)
}

func TestNewRejectsDuplicateDescriptor(t *testing.T) {
	_, err := New([]Descriptor{
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: " custom ",
			},
		},
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "custom",
			},
		},
	})

	require.Error(t, err)
	require.True(t, errors.Is(err, ErrDuplicateDescriptor))

	var duplicateErr DuplicateDescriptorError
	require.True(t, errors.As(err, &duplicateErr))
	require.Equal(t, devicetypes.ComponentTypeCompute, duplicateErr.ComponentType)
	require.Equal(t, "custom", duplicateErr.Implementation)
}

func TestSelectedDescriptors(t *testing.T) {
	catalog, err := New([]Descriptor{
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeNVSwitch,
				Implementation: "mock",
			},
			RequiredProviders: []string{},
		},
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypePowerShelf,
				Implementation: "multi-provider",
			},
			RequiredProviders: []string{
				"beta",
				"alpha",
			},
		},
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "custom",
			},
			RequiredProviders: []string{
				"zeta",
				"alpha",
			},
		},
	})
	require.NoError(t, err)

	descriptors, err := catalog.SelectedDescriptors(map[devicetypes.ComponentType]string{
		devicetypes.ComponentTypePowerShelf: "multi-provider",
		devicetypes.ComponentTypeNVSwitch:   "mock",
		devicetypes.ComponentTypeCompute:    "custom",
	})

	require.NoError(t, err)
	require.Equal(t, []Descriptor{
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "custom",
			},
			RequiredProviders: []string{
				"alpha",
				"zeta",
			},
		},
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeNVSwitch,
				Implementation: "mock",
			},
			RequiredProviders: []string{},
		},
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypePowerShelf,
				Implementation: "multi-provider",
			},
			RequiredProviders: []string{
				"alpha",
				"beta",
			},
		},
	}, descriptors)
}

func TestSelectedDescriptorsReturnsDescriptorCopies(t *testing.T) {
	catalog, err := New([]Descriptor{
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "custom",
			},
			RequiredProviders: []string{"alpha", "beta"},
			Capabilities: capability.CapabilitySet{
				capability.CapabilityFirmwareControl,
				capability.CapabilityPowerControl,
			},
		},
	})
	require.NoError(t, err)

	descriptors, err := catalog.SelectedDescriptors(map[devicetypes.ComponentType]string{
		devicetypes.ComponentTypeCompute: "custom",
	})
	require.NoError(t, err)
	require.Len(t, descriptors, 1)
	descriptors[0].RequiredProviders[0] = "mutated"
	descriptors[0].Capabilities[0] = "Mutated"

	descriptors, err = catalog.SelectedDescriptors(map[devicetypes.ComponentType]string{
		devicetypes.ComponentTypeCompute: "custom",
	})
	require.NoError(t, err)
	require.Equal(t, []Descriptor{
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "custom",
			},
			RequiredProviders: []string{"alpha", "beta"},
			Capabilities: capability.CapabilitySet{
				capability.CapabilityFirmwareControl,
				capability.CapabilityPowerControl,
			},
		},
	}, descriptors)
}

func TestSelectedDescriptorsAllowsEmptySelection(t *testing.T) {
	catalog, err := New([]Descriptor{
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "custom",
			},
		},
	})
	require.NoError(t, err)

	descriptors, err := catalog.SelectedDescriptors(nil)

	require.NoError(t, err)
	require.Empty(t, descriptors)
}

func TestSelectedDescriptorsRejectsUnregisteredComponentType(t *testing.T) {
	catalog, err := New([]Descriptor{
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "custom",
			},
		},
	})
	require.NoError(t, err)

	descriptors, err := catalog.SelectedDescriptors(map[devicetypes.ComponentType]string{
		devicetypes.ComponentTypeUMS: "custom",
	})

	require.Nil(t, descriptors)
	require.Error(t, err)
	require.True(t, errors.Is(err, ErrComponentManagerFactoryNotRegistered))

	var factoryErr ComponentManagerFactoryNotRegisteredError
	require.True(t, errors.As(err, &factoryErr))
	require.Equal(t, devicetypes.ComponentTypeUMS, factoryErr.ComponentType)
}

func TestSelectedDescriptorsRejectsUnknownImplementation(t *testing.T) {
	catalog, err := New([]Descriptor{
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "known",
			},
		},
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "alternate",
			},
		},
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypeNVSwitch,
				Implementation: "switch",
			},
		},
		{
			DescriptorIdentity: DescriptorIdentity{
				Type:           devicetypes.ComponentTypePowerShelf,
				Implementation: "switch",
			},
		},
	})
	require.NoError(t, err)

	descriptors, err := catalog.SelectedDescriptors(map[devicetypes.ComponentType]string{
		devicetypes.ComponentTypeCompute: "switch",
	})

	require.Nil(t, descriptors)
	require.Error(t, err)
	require.True(t, errors.Is(err, ErrUnknownComponentManagerImplementation))

	var implErr UnknownComponentManagerImplementationError
	require.True(t, errors.As(err, &implErr))
	require.Equal(t, devicetypes.ComponentTypeCompute, implErr.ComponentType)
	require.Equal(t, "switch", implErr.Implementation)
	require.Equal(t, []string{"alternate", "known"}, implErr.Available)
	require.Equal(t, []devicetypes.ComponentType{
		devicetypes.ComponentTypeNVSwitch,
		devicetypes.ComponentTypePowerShelf,
	}, implErr.RegisteredFor)
}
