// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package packages provides firmware package definition and loading.
package packages

// FirmwarePackage represents a firmware bundle defined in YAML.
// The Version field serves as the unique identifier for the bundle.
type FirmwarePackage struct {
	// Version is the unique identifier for this bundle (e.g., "1.0.0", "2024.01.15")
	Version string `yaml:"version"`

	// Description provides human-readable info about this bundle
	Description string `yaml:"description,omitempty"`

	// ComponentOrder defines the update sequence for components in this bundle.
	// Components are updated in the order listed. If not specified, uses DefaultComponentOrder.
	// e.g., ["bmc", "cpld", "bios", "nvos"]
	ComponentOrder []string `yaml:"component_order,omitempty"`

	// Components maps component name (lowercase) to its definition
	// e.g., "bmc", "cpld", "bios", "nvos"
	Components map[string]ComponentDef `yaml:"components"`

	// StrategyConfig contains configuration for each update strategy
	StrategyConfig StrategyConfig `yaml:"strategy_config,omitempty"`
}

// ComponentDef defines a single component within a firmware bundle.
type ComponentDef struct {
	// Version of this specific component
	Version string `yaml:"version"`

	// File is the relative path to the firmware file within the firmware directory
	File string `yaml:"file"`

	// Checksum for integrity verification (optional, format: "sha256:abc123...")
	Checksum string `yaml:"checksum,omitempty"`

	// Strategy specifies how this component is updated: "redfish", "ssh", or "script"
	Strategy string `yaml:"strategy"`

	// Script is the path to the update script (required when strategy is "script")
	// Can be absolute or relative to the scripts directory
	Script string `yaml:"script,omitempty"`

	// ScriptArgs specifies which arguments to pass to the script (when strategy is "script")
	// Each item is a token that resolves to a value at runtime.
	// Valid tokens:
	//   - bmc_ip, bmc_user, bmc_password
	//   - nvos_ip, nvos_user, nvos_password
	//   - fw_file (path to firmware file)
	// Example: [nvos_ip, nvos_user, nvos_password, fw_file]
	ScriptArgs []string `yaml:"script_args,omitempty"`
}

// StrategyConfig contains configuration for each update strategy.
type StrategyConfig struct {
	Script  *ScriptConfig  `yaml:"script,omitempty"`
	SSH     *SSHConfig     `yaml:"ssh,omitempty"`
	Redfish *RedfishConfig `yaml:"redfish,omitempty"`
}

// ScriptConfig contains configuration for script-based updates.
type ScriptConfig struct {
	// ScriptDir is the directory containing update scripts
	ScriptDir string `yaml:"script_dir"`

	// Timeout for script execution in seconds
	TimeoutSeconds int `yaml:"timeout_seconds,omitempty"`
}

// SSHConfig contains configuration for SSH-based updates.
type SSHConfig struct {
	// RemoteDir is the directory on the switch where files are copied
	RemoteDir string `yaml:"remote_dir,omitempty"`

	// RebootTimeoutSeconds is how long to wait for reboot (NVOS updates)
	RebootTimeoutSeconds int `yaml:"reboot_timeout_seconds,omitempty"`
}

// RedfishConfig contains configuration for Redfish-based updates.
type RedfishConfig struct {
	// PollIntervalSeconds is how often to poll task status
	PollIntervalSeconds int `yaml:"poll_interval_seconds,omitempty"`

	// PollTimeoutSeconds is max time to wait for task completion
	PollTimeoutSeconds int `yaml:"poll_timeout_seconds,omitempty"`
}

// GetComponent returns the component definition for the given component name.
// Returns nil if the component is not found.
func (p *FirmwarePackage) GetComponent(name string) *ComponentDef {
	if comp, ok := p.Components[name]; ok {
		return &comp
	}
	return nil
}

// HasComponent returns true if the package contains the given component.
func (p *FirmwarePackage) HasComponent(name string) bool {
	_, ok := p.Components[name]
	return ok
}

// DefaultComponentOrder defines the fallback update order when not specified in YAML.
// Update sequence: BMC → CPLD → BIOS → NVOS
var DefaultComponentOrder = []string{"bmc", "cpld", "bios", "nvos"}

// GetOrderedComponents returns the components in this package in the correct update order.
// Uses the package's ComponentOrder if defined, otherwise falls back to DefaultComponentOrder.
// Only components present in the package are returned.
func (p *FirmwarePackage) GetOrderedComponents() []string {
	// Use package-defined order if specified, otherwise use default
	order := p.ComponentOrder
	if len(order) == 0 {
		order = DefaultComponentOrder
	}

	var result []string
	for _, name := range order {
		if _, ok := p.Components[name]; ok {
			result = append(result, name)
		}
	}
	return result
}

// Validate checks that the package definition is valid.
func (p *FirmwarePackage) Validate() error {
	if p.Version == "" {
		return &ValidationError{Field: "version", Message: "version is required"}
	}

	if len(p.Components) == 0 {
		return &ValidationError{Field: "components", Message: "at least one component is required"}
	}

	// Validate component_order references valid components
	for _, name := range p.ComponentOrder {
		if _, ok := p.Components[name]; !ok {
			return &ValidationError{
				Field:   "component_order",
				Message: "references unknown component: " + name,
			}
		}
	}

	for name, comp := range p.Components {
		if comp.Version == "" {
			return &ValidationError{Field: "components." + name + ".version", Message: "version is required"}
		}
		if comp.File == "" {
			return &ValidationError{Field: "components." + name + ".file", Message: "file is required"}
		}
		if comp.Strategy == "" {
			return &ValidationError{Field: "components." + name + ".strategy", Message: "strategy is required"}
		}
		switch comp.Strategy {
		case "redfish", "ssh":
			// valid
		case "script":
			// Script strategy requires a script path
			if comp.Script == "" {
				return &ValidationError{
					Field:   "components." + name + ".script",
					Message: "script path is required when strategy is 'script'",
				}
			}
			// Validate each script_args token
			validTokens := map[string]bool{
				"bmc_ip": true, "bmc_user": true, "bmc_password": true,
				"nvos_ip": true, "nvos_user": true, "nvos_password": true,
				"fw_file": true,
			}
			for _, token := range comp.ScriptArgs {
				if !validTokens[token] {
					return &ValidationError{
						Field:   "components." + name + ".script_args",
						Message: "invalid token '" + token + "'; valid tokens: bmc_ip, bmc_user, bmc_password, nvos_ip, nvos_user, nvos_password, fw_file",
					}
				}
			}
		default:
			return &ValidationError{
				Field:   "components." + name + ".strategy",
				Message: "must be 'redfish', 'ssh', or 'script'",
			}
		}
	}

	return nil
}

// ValidationError represents a validation error in the package definition.
type ValidationError struct {
	Field   string
	Message string
}

func (e *ValidationError) Error() string {
	return "invalid firmware package: " + e.Field + ": " + e.Message
}
