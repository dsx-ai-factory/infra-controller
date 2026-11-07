// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"bytes"
	"fmt"
	"os"
	"strings"

	"gopkg.in/yaml.v3"

	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// LoadConfig loads the component manager configuration from a YAML file using
// the supplied provider config decoders, manager config decoders, and component
// manager catalog.
func LoadConfig(
	path string,
	decoders *providerapi.ProviderConfigDecoderRegistry,
	managerCatalog cmcatalog.Catalog,
	managerDecoderRegistry *ManagerConfigDecoderRegistry,
) (Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return Config{}, fmt.Errorf("failed to read config file: %w", err)
	}

	return ParseConfig(data, decoders, managerCatalog, managerDecoderRegistry)
}

// ParseConfig parses the component manager configuration from YAML data using
// the supplied provider config decoders, manager config decoders, and component
// manager catalog.
func ParseConfig(
	data []byte,
	decoders *providerapi.ProviderConfigDecoderRegistry,
	managerCatalog cmcatalog.Catalog,
	managerDecoderRegistry *ManagerConfigDecoderRegistry,
) (Config, error) {
	if decoders == nil {
		return Config{}, ErrProviderConfigDecoderRegistryRequired
	}

	rawComponentManagers, rawManagerConfigs, rawProviders, err := parseConfigYAML(data)
	if err != nil {
		return Config{}, err
	}

	config := newConfig()

	if err := setYAMLComponentManagers(&config, rawComponentManagers); err != nil {
		return Config{}, err
	}

	if err := setYAMLManagerConfigs(
		&config,
		rawManagerConfigs,
		managerDecoderRegistry,
		managerCatalog,
	); err != nil {
		return Config{}, err
	}

	if err := setYAMLProviderConfigs(&config, rawProviders, decoders); err != nil {
		return Config{}, err
	}

	if err := config.completeProviderConfigs(decoders, managerCatalog); err != nil {
		return Config{}, err
	}

	if err := config.completeManagerConfigs(
		managerDecoderRegistry,
		managerCatalog,
	); err != nil {
		return Config{}, err
	}

	return config, nil
}

// parseConfigYAML decodes only the generic YAML envelope. Provider-specific
// and manager-specific YAML remains as raw nodes so each decoder owns its own
// schema.
func parseConfigYAML(
	data []byte,
) (map[string]string, map[string]map[string]yaml.Node, map[string]yaml.Node, error) {
	var raw struct {
		ComponentManagers map[string]string               `yaml:"component_managers"`
		ManagerConfigs    map[string]map[string]yaml.Node `yaml:"manager_configs"`
		Providers         map[string]yaml.Node            `yaml:"providers"`
	}

	decoder := yaml.NewDecoder(bytes.NewReader(data))
	decoder.KnownFields(true)
	if err := decoder.Decode(&raw); err != nil {
		return nil, nil, nil, fmt.Errorf("failed to parse config: %w", err)
	}

	return raw.ComponentManagers, raw.ManagerConfigs, raw.Providers, nil
}

// setYAMLComponentManagers converts YAML component type keys to typed
// component types before adding them to the config.
func setYAMLComponentManagers(
	config *Config,
	rawComponentManagers map[string]string,
) error {
	for typeStr, implName := range rawComponentManagers {
		ct := devicetypes.ComponentTypeFromString(typeStr)
		if ct == devicetypes.ComponentTypeUnknown {
			return UnknownComponentTypeError{Name: typeStr}
		}

		if err := config.addComponentManager(ct, implName); err != nil {
			return err
		}
	}
	return nil
}

// setYAMLManagerConfigs decodes explicitly configured manager overrides from
// the manager_configs YAML section. Missing manager configs are intentionally
// handled later by completeManagerConfigs.
func setYAMLManagerConfigs(
	config *Config,
	rawManagerConfigs map[string]map[string]yaml.Node,
	decoders *ManagerConfigDecoderRegistry,
	managerCatalog cmcatalog.Catalog,
) error {
	if len(rawManagerConfigs) == 0 {
		return nil
	}

	if decoders == nil {
		return ErrManagerConfigDecoderRegistryRequired
	}

	for rawType, rawImplementations := range rawManagerConfigs {
		ct := devicetypes.ComponentTypeFromString(strings.TrimSpace(rawType))
		if ct == devicetypes.ComponentTypeUnknown {
			return UnknownComponentTypeError{Name: rawType}
		}

		for rawImplName, rawNode := range rawImplementations {
			identity, err := (cmcatalog.DescriptorIdentity{
				Type:           ct,
				Implementation: rawImplName,
			}).Normalize()
			if err != nil {
				return err
			}

			selectedImplementation := config.ComponentManagers[identity.Type]
			if selectedImplementation != identity.Implementation {
				return ManagerConfigNotSelectedError{
					Identity:               identity,
					SelectedImplementation: selectedImplementation,
				}
			}

			if _, ok := managerCatalog.Get(identity); !ok {
				return cmcatalog.UnknownComponentManagerImplementationError{
					ComponentType:  identity.Type,
					Implementation: identity.Implementation,
					Available:      managerCatalog.Implementations(identity.Type),
				}
			}

			decoder := decoders.Get(identity)
			if decoder == nil {
				return ManagerConfigDecoderNotRegisteredError{
					Identity: identity,
				}
			}

			managerConfig, err := decoder.DecodeYAML(rawNode)
			if err != nil {
				return fmt.Errorf("decode manager config %s: %w", identity, err)
			}

			if err := config.addManagerConfig(identity, managerConfig); err != nil {
				return fmt.Errorf("add manager config %s: %w", identity, err)
			}
		}
	}

	return nil
}

// setYAMLProviderConfigs decodes explicitly configured provider overrides.
// Missing required providers are intentionally handled later by
// completeProviderConfigs.
func setYAMLProviderConfigs(
	config *Config,
	rawProviders map[string]yaml.Node,
	decoders *providerapi.ProviderConfigDecoderRegistry,
) error {
	for rawName, rawNode := range rawProviders {
		name, decoder, err := config.prepareProviderConfigForAdd(rawName, decoders)
		if err != nil {
			return err
		}

		if config.HasProvider(name) {
			return DuplicateProviderConfigError{Name: name}
		}

		providerConfig, err := decoder.DecodeYAML(rawNode)
		if err != nil {
			return fmt.Errorf("provider %q: %w", name, err)
		}

		config.ProviderConfigs[name] = providerConfig
	}

	return nil
}
