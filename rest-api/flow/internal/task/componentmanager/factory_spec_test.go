// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package componentmanager

import (
	"errors"
	"testing"

	"github.com/stretchr/testify/require"

	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestSelectFactorySpecsReturnsSelectedDescriptors(t *testing.T) {
	descriptors, factories, err := selectFactorySpecs(
		[]FactorySpec{
			testFactorySpec(
				devicetypes.ComponentTypePowerShelf,
				"psm",
				managerFactory(devicetypes.ComponentTypePowerShelf, "psm"),
				"beta",
				"alpha",
			),
			testFactorySpec(
				devicetypes.ComponentTypeCompute,
				" custom ",
				managerFactory(devicetypes.ComponentTypeCompute, "custom"),
				" nico ",
				"nico",
			),
			testFactorySpec(
				devicetypes.ComponentTypeCompute,
				"unused",
				managerFactory(devicetypes.ComponentTypeCompute, "unused"),
			),
		},
		map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypePowerShelf: "psm",
			devicetypes.ComponentTypeCompute:    "custom",
		},
	)

	require.NoError(t, err)
	require.Equal(t, []cmcatalog.Descriptor{
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypeCompute,
				Implementation: "custom",
			},
			RequiredProviders: []string{"nico"},
		},
		{
			DescriptorIdentity: cmcatalog.DescriptorIdentity{
				Type:           devicetypes.ComponentTypePowerShelf,
				Implementation: "psm",
			},
			RequiredProviders: []string{
				"alpha",
				"beta",
			},
		},
	}, descriptors)
	require.NotNil(t, factories[descriptorKeyOf(descriptors[0])])
	require.NotNil(t, factories[descriptorKeyOf(descriptors[1])])
}

func TestSelectFactorySpecsAllowsEmptySelection(t *testing.T) {
	descriptors, factories, err := selectFactorySpecs(
		[]FactorySpec{
			testFactorySpec(
				devicetypes.ComponentTypeCompute,
				"custom",
				managerFactory(devicetypes.ComponentTypeCompute, "custom"),
			),
		},
		nil,
	)

	require.NoError(t, err)
	require.Empty(t, descriptors)
	require.Len(t, factories, 1)
}

func TestSelectFactorySpecsRejectsInvalidFactorySpec(t *testing.T) {
	descriptors, factories, err := selectFactorySpecs(
		[]FactorySpec{
			{
				Descriptor: testDescriptor(
					devicetypes.ComponentTypeCompute,
					"custom",
				),
			},
		},
		map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypeCompute: "custom",
		},
	)

	require.Nil(t, descriptors)
	require.Nil(t, factories)
	require.Error(t, err)
	require.True(t, errors.Is(err, ErrComponentManagerFactoryNotConfigured))
}

func TestSelectFactorySpecsRejectsDuplicateDescriptor(t *testing.T) {
	descriptors, factories, err := selectFactorySpecs(
		[]FactorySpec{
			testFactorySpec(
				devicetypes.ComponentTypeCompute,
				" custom ",
				managerFactory(devicetypes.ComponentTypeCompute, "custom"),
			),
			testFactorySpec(
				devicetypes.ComponentTypeCompute,
				"custom",
				managerFactory(devicetypes.ComponentTypeCompute, "custom"),
			),
		},
		map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypeCompute: "custom",
		},
	)

	require.Nil(t, descriptors)
	require.Nil(t, factories)
	require.Error(t, err)
	require.True(t, errors.Is(err, ErrDuplicateDescriptor))

	var duplicateErr DuplicateDescriptorError
	require.True(t, errors.As(err, &duplicateErr))
	require.Equal(t, devicetypes.ComponentTypeCompute, duplicateErr.ComponentType)
	require.Equal(t, "custom", duplicateErr.Implementation)
}

func TestSelectFactorySpecsRejectsUnknownImplementation(t *testing.T) {
	descriptors, factories, err := selectFactorySpecs(
		[]FactorySpec{
			testFactorySpec(
				devicetypes.ComponentTypeCompute,
				"known",
				managerFactory(devicetypes.ComponentTypeCompute, "known"),
			),
			testFactorySpec(
				devicetypes.ComponentTypeNVSwitch,
				"switch",
				managerFactory(devicetypes.ComponentTypeNVSwitch, "switch"),
			),
		},
		map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypeCompute: "switch",
		},
	)

	require.Nil(t, descriptors)
	require.Nil(t, factories)
	require.Error(t, err)
	require.True(t, errors.Is(err, ErrUnknownComponentManagerImplementation))

	var implErr UnknownComponentManagerImplementationError
	require.True(t, errors.As(err, &implErr))
	require.Equal(t, devicetypes.ComponentTypeCompute, implErr.ComponentType)
	require.Equal(t, "switch", implErr.Implementation)
	require.Equal(t, []string{"known"}, implErr.Available)
	require.Equal(t, []devicetypes.ComponentType{
		devicetypes.ComponentTypeNVSwitch,
	}, implErr.RegisteredFor)
}
