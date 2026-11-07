// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicolegacy

import (
	"time"

	"gopkg.in/yaml.v3"

	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

const (
	// DefaultComputePowerDelay is the default delay between sequential power
	// control calls for compute trays. A small stagger avoids overwhelming the
	// power delivery system.
	DefaultComputePowerDelay = 2 * time.Second
)

// Config holds manager-specific configuration for compute/nicolegacy.
type Config struct {
	// ComputePowerDelay is the delay inserted between sequential power control
	// calls when commanding multiple compute trays. 0 means no delay.
	ComputePowerDelay time.Duration
}

type rawConfig struct {
	ComputePowerDelay string `yaml:"compute_power_delay"`
}

// Validate verifies that this config is used with the compute/nicolegacy
// descriptor.
func (*Config) Validate(expectedIdentity cmcatalog.DescriptorIdentity) error {
	actualIdentity := ConfigDecoder{}.Identity()
	if expectedIdentity != actualIdentity {
		return cmconfig.ManagerConfigIdentityMismatchError{
			Expected: expectedIdentity,
			Actual:   actualIdentity,
		}
	}
	return nil
}

// ConfigDecoder owns compute/nicolegacy manager config defaults and YAML
// decoding.
type ConfigDecoder struct{}

// Identity returns the descriptor identity handled by this decoder.
func (ConfigDecoder) Identity() cmcatalog.DescriptorIdentity {
	return cmcatalog.DescriptorIdentity{
		Type:           devicetypes.ComponentTypeCompute,
		Implementation: ImplementationName,
	}
}

// DefaultConfig returns the default compute/nicolegacy manager config.
func (ConfigDecoder) DefaultConfig() cmconfig.ManagerConfig {
	return &Config{
		ComputePowerDelay: DefaultComputePowerDelay,
	}
}

// DecodeYAML decodes compute/nicolegacy manager YAML into a typed config.
func (d ConfigDecoder) DecodeYAML(raw yaml.Node) (cmconfig.ManagerConfig, error) {
	config := d.DefaultConfig().(*Config)

	var parsed rawConfig
	err := cmconfig.DecodeYAMLStrict(raw, &parsed)
	if err != nil {
		return nil, cmconfig.InvalidManagerConfigError{
			Identity: d.Identity(),
			Err:      err,
		}
	}

	if parsed.ComputePowerDelay != "" {
		delay, err := time.ParseDuration(parsed.ComputePowerDelay)
		if err != nil {
			return nil, cmconfig.InvalidManagerConfigFieldError{
				Identity: d.Identity(),
				Field:    "compute_power_delay",
				Err:      err,
			}
		}
		config.ComputePowerDelay = delay
	}

	return config, nil
}
