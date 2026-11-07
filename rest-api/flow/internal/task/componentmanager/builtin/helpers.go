// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package builtin

import (
	"fmt"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	computenicolegacy "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/compute/nicolegacy"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
)

// newProviderDecoderRegistry creates the provider config decoder registry used
// by the Flow service.
func newProviderDecoderRegistry() (*providerapi.ProviderConfigDecoderRegistry, error) {
	registry := providerapi.NewProviderConfigDecoderRegistry()

	for _, decoder := range serviceProviderConfigDecoders() {
		if err := registry.Register(decoder); err != nil {
			return nil, fmt.Errorf(
				"register service provider config decoder %q: %w",
				decoder.Name(),
				err,
			)
		}
	}

	return registry, nil
}

// newManagerConfigDecoderRegistry creates the manager config decoder registry
// used by the Flow service.
func newManagerConfigDecoderRegistry() (*cmconfig.ManagerConfigDecoderRegistry, error) {
	registry := cmconfig.NewManagerConfigDecoderRegistry()

	for _, decoder := range serviceManagerConfigDecoders() {
		if err := registry.Register(decoder); err != nil {
			return nil, fmt.Errorf(
				"register service manager config decoder %q: %w",
				managerConfigDecoderName(decoder),
				err,
			)
		}
	}

	return registry, nil
}

// newCatalog builds the component manager catalog for the Flow service.
// The catalog contains the descriptors for all the built-in component managers
// supported by the Flow service.
func newCatalog() (cmcatalog.Catalog, error) {
	catalog, err := cmcatalog.New(serviceDescriptors())
	if err != nil {
		return cmcatalog.Catalog{}, fmt.Errorf(
			"build component manager catalog: %w",
			err,
		)
	}

	return catalog, nil
}

// legacyComputePowerDelay extracts the power-delay manager config for the
// compute/nicolegacy implementation. This setting only applies to the legacy
// path (which serializes per-machine AdminPowerControl calls); the
// replacement compute/nico path issues a single bulk ComponentPowerControl
// and therefore does not need it.
func legacyComputePowerDelay(config cmconfig.Config) (time.Duration, error) {
	identity := computenicolegacy.Descriptor().Identity()
	managerConfig, ok := config.ManagerConfigs[identity]
	if !ok {
		return computenicolegacy.DefaultComputePowerDelay, nil
	}
	if managerConfig == nil {
		return 0, cmconfig.ManagerConfigNotConfiguredError{Identity: identity}
	}

	legacyConfig, ok := managerConfig.(*computenicolegacy.Config)
	if !ok {
		return 0, componentmanager.ManagerConfigTypeMismatchError{
			Identity: identity,
			Got:      managerConfig,
			Want:     (*computenicolegacy.Config)(nil),
		}
	}
	return legacyConfig.ComputePowerDelay, nil
}

func managerConfigDecoderName(decoder cmconfig.ManagerConfigDecoder) string {
	return decoder.Identity().String()
}
