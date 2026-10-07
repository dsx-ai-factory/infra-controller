// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package builtin

import (
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	computenico "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/compute/nico"
	computenicolegacy "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/compute/nicolegacy"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/mock"
	nvswitchnico "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/nvswitch/nico"
	powershelfnico "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/powershelf/nico"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	nicoprovider "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providers/nico"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/readiness"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// This file is the Flow service component manager manifest. When adding a
// provider or component manager implementation compiled into the service, add
// its default mapping, decoder, descriptor, and runtime factory spec entries here.

// defaultServiceComponentManagers returns the component manager implementation
// map used when the Flow service is started without a component manager config
// file. A configured file is authoritative and does not merge with this map.
func defaultServiceComponentManagers() map[devicetypes.ComponentType]string {
	return map[devicetypes.ComponentType]string{
		devicetypes.ComponentTypeCompute:    computenico.ImplementationName,
		devicetypes.ComponentTypeNVSwitch:   nvswitchnico.ImplementationName,
		devicetypes.ComponentTypePowerShelf: powershelfnico.ImplementationName,
	}
}

// serviceProviderConfigDecoders returns all provider config decoders supported
// by the Flow service.
func serviceProviderConfigDecoders() []providerapi.ProviderConfigDecoder {
	return []providerapi.ProviderConfigDecoder{
		nicoprovider.ConfigDecoder{},
	}
}

// serviceManagerConfigDecoders returns all manager config decoders supported
// by the Flow service.
func serviceManagerConfigDecoders() []cmconfig.ManagerConfigDecoder {
	return []cmconfig.ManagerConfigDecoder{
		computenicolegacy.ConfigDecoder{},
	}
}

// serviceDescriptors returns all component manager descriptors compiled into
// the Flow service. Descriptors stay in a separate manifest even though
// FactorySpec also contains a Descriptor because descriptor metadata is needed
// before runtime factories are built. Config validation and provider dependency
// discovery can use this static list without constructing config-dependent
// factory specs.
func serviceDescriptors() []cmcatalog.Descriptor {
	descriptors := []cmcatalog.Descriptor{
		computenico.Descriptor(),
		computenicolegacy.Descriptor(),
		nvswitchnico.Descriptor(),
		powershelfnico.Descriptor(),
	}

	descriptors = append(descriptors, mock.Descriptors()...)
	return descriptors
}

// serviceFactorySpecs returns all runtime component manager factory specs
// compiled into the Flow service. Factory specs pair descriptors with factories
// after service config has been decoded, so this manifest is intentionally
// separate from serviceDescriptors for managers whose factories need
// config-derived values. gate is the shared readiness gate every NICo-backed
// manager consults before disruptive operations; a nil gate is permissive
// (used by tests that do not exercise readiness behaviour).
func serviceFactorySpecs(
	config cmconfig.Config,
	gate readiness.Gate,
) ([]componentmanager.FactorySpec, error) {
	computePowerDelay, err := legacyComputePowerDelay(config)
	if err != nil {
		return nil, err
	}

	factorySpecs := []componentmanager.FactorySpec{
		computenico.FactorySpec(gate),
		computenicolegacy.FactorySpec(computePowerDelay, gate),
		nvswitchnico.FactorySpec(gate),
		powershelfnico.FactorySpec(gate),
	}

	factorySpecs = append(factorySpecs, mock.FactorySpecs()...)
	return factorySpecs, nil
}
