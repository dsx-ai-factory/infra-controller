// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package componentmanager

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// testManager intentionally implements only ComponentManager descriptor
// metadata. Tests that need operation interfaces should use focused helpers,
// such as capabilityTestManager or descriptorOnlyManager in activity tests.
type testManager struct {
	descriptor cmcatalog.Descriptor
}

func (m testManager) Descriptor() cmcatalog.Descriptor {
	return m.descriptor
}

func managerFactory(
	componentType devicetypes.ComponentType,
	implementation string,
	requiredProviders ...string,
) ManagerFactory {
	return func(*providerapi.ProviderRegistry) (ComponentManager, error) {
		return testManager{
			descriptor: testDescriptor(
				componentType,
				implementation,
				requiredProviders...,
			),
		}, nil
	}
}

func testDescriptor(
	componentType devicetypes.ComponentType,
	implementation string,
	requiredProviders ...string,
) cmcatalog.Descriptor {
	return cmcatalog.Descriptor{
		DescriptorIdentity: cmcatalog.DescriptorIdentity{
			Type:           componentType,
			Implementation: implementation,
		},
		RequiredProviders: requiredProviders,
	}
}

func testFactorySpec(
	componentType devicetypes.ComponentType,
	implementation string,
	factory ManagerFactory,
	requiredProviders ...string,
) FactorySpec {
	return FactorySpec{
		Descriptor: testDescriptor(
			componentType,
			implementation,
			requiredProviders...,
		),
		Factory: factory,
	}
}

func newRegistryWithCapabilities(
	t *testing.T,
	capabilities ...capability.Capability,
) *Registry {
	t.Helper()

	descriptor := cmcatalog.Descriptor{
		DescriptorIdentity: cmcatalog.DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "custom",
		},
		Capabilities: capability.CapabilitySet(capabilities),
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
	if err != nil {
		t.Fatalf("NewRegistry() error = %v", err)
	}

	return registry
}
