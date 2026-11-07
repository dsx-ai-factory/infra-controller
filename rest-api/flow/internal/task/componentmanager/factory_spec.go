// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package componentmanager

import (
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// ManagerFactory creates a ComponentManager instance from the provider
// registry. Implementations retrieve any required providers from the supplied
// registry.
type ManagerFactory func(providers *providerapi.ProviderRegistry) (ComponentManager, error)

// FactorySpec describes a component manager implementation that can be created
// at runtime. Descriptor contains selection and validation metadata; Factory
// creates the manager after config and providers are ready.
type FactorySpec struct {
	Descriptor cmcatalog.Descriptor
	Factory    ManagerFactory
}

type descriptorKey struct {
	componentType  devicetypes.ComponentType
	implementation string
}

func selectFactorySpecs(
	factorySpecs []FactorySpec,
	componentManagers map[devicetypes.ComponentType]string,
) ([]cmcatalog.Descriptor, map[descriptorKey]ManagerFactory, error) {
	factories := make(map[descriptorKey]ManagerFactory, len(factorySpecs))
	descriptors := make([]cmcatalog.Descriptor, 0, len(factorySpecs))

	for _, factorySpec := range factorySpecs {
		spec, err := factorySpec.normalize()
		if err != nil {
			return nil, nil, err
		}

		descriptor := spec.Descriptor
		descriptors = append(descriptors, descriptor)
		factories[descriptorKeyOf(descriptor)] = spec.Factory
	}

	catalog, err := cmcatalog.New(descriptors)
	if err != nil {
		return nil, nil, err
	}

	selectedDescriptors, err := catalog.SelectedDescriptors(componentManagers)
	if err != nil {
		return nil, nil, err
	}

	return selectedDescriptors, factories, nil
}

func descriptorKeyOf(descriptor cmcatalog.Descriptor) descriptorKey {
	return descriptorKey{
		componentType:  descriptor.Type,
		implementation: descriptor.Implementation,
	}
}

func (s FactorySpec) normalize() (FactorySpec, error) {
	descriptor, err := s.Descriptor.Normalize()
	if err != nil {
		return FactorySpec{}, err
	}

	if s.Factory == nil {
		return FactorySpec{}, ComponentManagerFactoryNotConfiguredError{
			ComponentType:  descriptor.Type,
			Implementation: descriptor.Implementation,
		}
	}

	s.Descriptor = descriptor
	return s, nil
}
