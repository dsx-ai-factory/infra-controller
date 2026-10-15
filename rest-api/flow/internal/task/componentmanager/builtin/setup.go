// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package builtin wires the component manager extensions compiled into the
// Flow binary.
package builtin

import (
	"context"
	"errors"
	"fmt"
	"io"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/readiness"
)

// LoadConfig loads the component manager config for the Flow service.
// If path is empty, the embedded service defaults are used. If path is set, the
// YAML file is authoritative and must satisfy service config validation.
func LoadConfig(path string) (cmconfig.Config, error) {
	decoders, err := newProviderDecoderRegistry()
	if err != nil {
		return cmconfig.Config{}, fmt.Errorf(
			"initialize service provider config decoders: %w",
			err,
		)
	}

	managerDecoderRegistry, err := newManagerConfigDecoderRegistry()
	if err != nil {
		return cmconfig.Config{}, fmt.Errorf(
			"initialize service manager config decoders: %w",
			err,
		)
	}

	catalog, err := newCatalog()
	if err != nil {
		return cmconfig.Config{}, err
	}

	var config cmconfig.Config
	if path != "" {
		config, err = cmconfig.LoadConfig(
			path,
			decoders,
			catalog,
			managerDecoderRegistry,
		)
		if err != nil {
			return cmconfig.Config{}, fmt.Errorf("load config from file: %w", err)
		}
	} else {
		config, err = cmconfig.New(
			defaultServiceComponentManagers(),
			decoders,
			catalog,
			managerDecoderRegistry,
		)
		if err != nil {
			return cmconfig.Config{}, fmt.Errorf("get default config: %w", err)
		}
	}

	err = config.Validate(decoders, catalog, managerDecoderRegistry)
	if err != nil {
		return cmconfig.Config{}, fmt.Errorf("validate loaded config: %w", err)
	}

	return config, nil
}

// NewProviderRegistry creates and initializes the Flow service provider
// registry from decoded provider configs.
func NewProviderRegistry(
	ctx context.Context,
	config cmconfig.Config,
) (_ *providerapi.ProviderRegistry, err error) {
	providerRegistry := providerapi.NewProviderRegistry()
	defer func() {
		if err != nil {
			err = errors.Join(err, providerRegistry.Close())
		}
	}()

	for name, providerConfig := range config.ProviderConfigs {
		// LoadConfig builds ProviderConfigs through service decoders, but keep
		// this defensive for Config values constructed in tests or by callers.
		if providerConfig == nil {
			return nil, providerapi.ProviderNotConfiguredError{Name: name}
		}
		configName := providerConfig.Name()
		if name != configName {
			return nil, providerapi.ProviderConfigNameMismatchError{
				Name:       name,
				ConfigName: configName,
			}
		}

		provider, err := providerConfig.NewProvider(ctx)
		if err != nil {
			return nil, fmt.Errorf("create provider %q: %w", name, err)
		}
		if provider == nil {
			return nil, providerapi.ProviderNotConfiguredError{Name: name}
		}

		providerName := provider.Name()
		if providerName != name {
			err = providerapi.ProviderNameMismatchError{
				Name:         name,
				ProviderName: providerName,
			}
			if closer, ok := provider.(io.Closer); ok {
				err = errors.Join(err, closer.Close())
			}
			return nil, err
		}
		err = providerRegistry.Register(provider)
		if err != nil {
			if closer, ok := provider.(io.Closer); ok {
				err = errors.Join(err, closer.Close())
			}
			return nil, err
		}
		log.Info().
			Str("provider", name).
			Msg("Initialized provider")
	}

	registeredProviders := providerRegistry.List()
	log.Info().
		Strs("providers", registeredProviders).
		Msg("Provider registry initialized")

	return providerRegistry, nil
}

// NewComponentManagerRegistry creates the component manager registry for the
// Flow service using all component manager implementations compiled into the
// binary. gate is the shared readiness gate that NICo-backed managers consult
// before disruptive operations; pass nil to skip readiness gating (e.g. in
// tests that do not exercise it).
func NewComponentManagerRegistry(
	config cmconfig.Config,
	providers *providerapi.ProviderRegistry,
	gate readiness.Gate,
) (*componentmanager.Registry, error) {
	factorySpecs, err := serviceFactorySpecs(config, gate)
	if err != nil {
		return nil, err
	}

	registry, err := componentmanager.NewRegistry(factorySpecs, config, providers)
	if err != nil {
		return nil, fmt.Errorf("initialize component managers: %w", err)
	}

	return registry, nil
}
