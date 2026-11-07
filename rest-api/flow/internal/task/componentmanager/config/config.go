// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package config loads, normalizes, and validates component manager
// configuration, including component-manager selections, manager-specific
// configs, and provider/client configs.
//
// Config can be built from YAML through LoadConfig or ParseConfig, or from
// embedded defaults through New. All constructor paths normalize component
// manager implementation names, decode explicit provider and manager config
// overrides, and complete missing configs from the component manager catalog
// supplied by the caller.
package config

import (
	"strings"

	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// Config holds the component manager configuration.
//
// Config values returned by ParseConfig, LoadConfig, and New are
// normalized: component manager implementation names are trimmed, unknown
// component types are rejected, explicit provider names are trimmed, duplicate
// provider names are rejected after trimming, and missing provider configs
// required by catalog descriptors are completed from provider defaults. Manager
// configs are keyed by descriptor identity and completed from manager defaults
// when a decoder is registered for the selected manager.
type Config struct {
	// ComponentManagers maps each component type to the component manager
	// implementation responsible for managing that type. Each component manager
	// implementation can use a provider to talk to its external service.
	ComponentManagers map[devicetypes.ComponentType]string

	// ManagerConfigs holds manager-specific typed configs keyed by descriptor
	// identity. These configs control manager behavior and are separate from
	// provider/client settings.
	ManagerConfigs map[cmcatalog.DescriptorIdentity]ManagerConfig

	// ProviderConfigs holds provider-specific typed configs keyed by provider
	// name. Explicit provider configs override defaults; missing providers
	// required by catalog descriptors are completed with provider defaults.
	// Providers are configured once and can be shared by multiple component
	// manager implementations.
	ProviderConfigs map[string]providerapi.ProviderConfig
}

// New builds a component manager config from a component-manager
// implementation map and derives default provider configs, plus default
// manager configs when manager decoders are supplied, from the catalog.
func New(
	componentManagers map[devicetypes.ComponentType]string,
	decoders *providerapi.ProviderConfigDecoderRegistry,
	managerCatalog cmcatalog.Catalog,
	managerDecoderRegistry *ManagerConfigDecoderRegistry,
) (Config, error) {
	if decoders == nil {
		return Config{}, ErrProviderConfigDecoderRegistryRequired
	}

	config := newConfig()

	for ct, implName := range componentManagers {
		if err := config.addComponentManager(ct, implName); err != nil {
			return Config{}, err
		}
	}

	err := config.completeProviderConfigs(decoders, managerCatalog)
	if err != nil {
		return Config{}, err
	}

	err = config.completeManagerConfigs(
		managerDecoderRegistry,
		managerCatalog,
	)
	if err != nil {
		return Config{}, err
	}

	return config, nil
}

// newConfig creates an empty normalized-config accumulator. Callers add
// component managers and providers through the package helpers so normalization
// stays centralized.
func newConfig() Config {
	return Config{
		ComponentManagers: make(map[devicetypes.ComponentType]string),
		ManagerConfigs:    make(map[cmcatalog.DescriptorIdentity]ManagerConfig),
		ProviderConfigs:   make(map[string]providerapi.ProviderConfig),
	}
}

// addComponentManager validates a component-manager entry and stores the
// normalized implementation name.
func (c *Config) addComponentManager(
	ct devicetypes.ComponentType,
	implName string,
) error {
	if ct == devicetypes.ComponentTypeUnknown {
		return UnknownComponentTypeError{
			Name: devicetypes.ComponentTypeToString(ct),
		}
	}

	implName = strings.TrimSpace(implName)
	if implName == "" {
		return ComponentManagerImplementationNameEmptyError{
			ComponentType: ct,
		}
	}

	c.ComponentManagers[ct] = implName
	return nil
}

// prepareProviderConfigForAdd normalizes a provider name, verifies the config
// does not already contain it, and resolves the decoder for that provider.
func (c *Config) prepareProviderConfigForAdd(
	name string,
	decoders *providerapi.ProviderConfigDecoderRegistry,
) (string, providerapi.ProviderConfigDecoder, error) {
	if c == nil {
		return "", nil, ErrConfigNotConfigured
	}

	if decoders == nil {
		return "", nil, ErrProviderConfigDecoderRegistryRequired
	}

	name = strings.TrimSpace(name)
	if name == "" {
		return "", nil, providerapi.ErrProviderNameEmpty
	}

	decoder, ok := decoders.Get(name)
	if !ok {
		return "", nil, providerapi.UnknownProviderError{Name: name}
	}

	return name, decoder, nil
}

// completeProviderConfigs enables missing providers based on the configured
// component manager catalog descriptors. Explicit provider configs already
// present in the config are preserved.
func (c *Config) completeProviderConfigs(
	decoders *providerapi.ProviderConfigDecoderRegistry,
	managerCatalog cmcatalog.Catalog,
) error {
	if decoders == nil {
		return ErrProviderConfigDecoderRegistryRequired
	}

	descriptors, err := managerCatalog.SelectedDescriptors(c.ComponentManagers)
	if err != nil {
		return err
	}

	for _, descriptor := range descriptors {
		for _, name := range descriptor.RequiredProviders {
			if c.HasProvider(name) {
				continue
			}

			decoder, ok := decoders.Get(name)
			if !ok {
				return ProviderConfigDecoderNotRegisteredError{
					Name:           name,
					ComponentType:  descriptor.Type,
					Implementation: descriptor.Implementation,
				}
			}

			c.ProviderConfigs[name] = decoder.DefaultConfig()
		}
	}

	return nil
}

// addManagerConfig validates a manager config and stores it by descriptor
// identity. Callers are responsible for passing a normalized expected identity.
func (c *Config) addManagerConfig(
	expectedIdentity cmcatalog.DescriptorIdentity,
	managerConfig ManagerConfig,
) error {
	if c == nil {
		return ErrConfigNotConfigured
	}

	if managerConfig == nil {
		return ManagerConfigNotConfiguredError{Identity: expectedIdentity}
	}

	if err := managerConfig.Validate(expectedIdentity); err != nil {
		return err
	}

	if c.ManagerConfigs == nil {
		c.ManagerConfigs = make(map[cmcatalog.DescriptorIdentity]ManagerConfig)
	}

	if _, exists := c.ManagerConfigs[expectedIdentity]; exists {
		return DuplicateManagerConfigError{Identity: expectedIdentity}
	}

	c.ManagerConfigs[expectedIdentity] = managerConfig
	return nil
}

// completeManagerConfigs enables missing manager configs for selected
// component managers when a manager config decoder is registered. Explicit
// manager configs already present in the config are preserved.
func (c *Config) completeManagerConfigs(
	decoders *ManagerConfigDecoderRegistry,
	managerCatalog cmcatalog.Catalog,
) error {
	if decoders == nil {
		return nil
	}

	descriptors, err := managerCatalog.SelectedDescriptors(c.ComponentManagers)
	if err != nil {
		return err
	}

	for _, descriptor := range descriptors {
		identity := descriptor.Identity()
		decoder := decoders.Get(identity)
		if decoder == nil {
			continue
		}

		if c.HasManagerConfig(identity) {
			continue
		}

		if err := c.addManagerConfig(identity, decoder.DefaultConfig()); err != nil {
			return err
		}
	}

	return nil
}

// Validate verifies the component manager config contract, including selected
// descriptors, required providers, and manager-specific configs.
func (c *Config) Validate(
	decoders *providerapi.ProviderConfigDecoderRegistry,
	managerCatalog cmcatalog.Catalog,
	managerDecoderRegistry *ManagerConfigDecoderRegistry,
) error {
	if c == nil {
		return ErrConfigNotConfigured
	}

	if len(c.ComponentManagers) == 0 {
		return ErrComponentManagersNotConfigured
	}

	if decoders == nil {
		return ErrProviderConfigDecoderRegistryRequired
	}

	descriptors, err := managerCatalog.SelectedDescriptors(c.ComponentManagers)
	if err != nil {
		return err
	}
	selectedIdentities := make(
		map[cmcatalog.DescriptorIdentity]struct{},
		len(descriptors),
	)

	for _, descriptor := range descriptors {
		selectedIdentities[descriptor.Identity()] = struct{}{}

		for _, providerName := range descriptor.RequiredProviders {
			if _, ok := decoders.Get(providerName); !ok {
				return ProviderConfigDecoderNotRegisteredError{
					Name:           providerName,
					ComponentType:  descriptor.Type,
					Implementation: descriptor.Implementation,
				}
			}

			if !c.HasProvider(providerName) {
				return RequiredProviderNotConfiguredError{
					Provider:       providerName,
					ComponentType:  descriptor.Type,
					Implementation: descriptor.Implementation,
				}
			}
		}
	}

	for identity, managerConfig := range c.ManagerConfigs {
		normalizedIdentity, err := identity.Normalize()
		if err != nil {
			return err
		}

		if identity != normalizedIdentity {
			return ManagerConfigIdentityMismatchError{
				Expected: normalizedIdentity,
				Actual:   identity,
			}
		}

		if _, ok := selectedIdentities[identity]; !ok {
			return ManagerConfigNotSelectedError{Identity: identity}
		}

		if managerDecoderRegistry != nil && managerDecoderRegistry.Get(identity) == nil {
			return ManagerConfigDecoderNotRegisteredError{Identity: identity}
		}

		if managerConfig == nil {
			return ManagerConfigNotConfiguredError{Identity: identity}
		}

		if err := managerConfig.Validate(identity); err != nil {
			return err
		}
	}

	return nil
}

// HasProvider checks if a provider is enabled in the configuration.
func (c *Config) HasProvider(name string) bool {
	if c != nil && c.ProviderConfigs != nil {
		if _, ok := c.ProviderConfigs[name]; ok {
			return true
		}
	}

	return false
}

// HasManagerConfig checks if a manager config is enabled for a normalized
// descriptor identity. HasManagerConfig does not normalize identity; callers
// should normalize identities built from raw input before calling. Passing an
// unnormalized identity simply misses and returns false.
func (c *Config) HasManagerConfig(identity cmcatalog.DescriptorIdentity) bool {
	if c != nil && c.ManagerConfigs != nil {
		if _, ok := c.ManagerConfigs[identity]; ok {
			return true
		}
	}

	return false
}
