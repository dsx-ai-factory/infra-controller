// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"fmt"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// actionSpec defines validation rules for each action type
type actionSpec struct {
	requiredParams       []string                   // Must be present in Parameters map
	optionalParams       []string                   // Can be present in Parameters map
	requiresPollInterval bool                       // Must have PollInterval field set
	requiresTimeout      bool                       // Must have Timeout field set
	implementation       string                     // Where action is implemented
	description          string                     // Human-readable description
	validateParams       func(map[string]any) error // Parameter validator
}

// actionRegistry maps action names to their validation specs
var actionRegistry = map[string]actionSpec{
	ActionSleep: {
		requiredParams:       []string{ParamDuration},
		optionalParams:       []string{},
		requiresPollInterval: false,
		requiresTimeout:      false,
		implementation:       "workflow.Sleep",
		description:          "Pause execution for specified duration",
		validateParams:       validateSleepParams,
	},
	ActionPowerControl: {
		requiredParams:       []string{},
		optionalParams:       []string{ParamOperation},
		requiresPollInterval: false,
		requiresTimeout:      false, // Uses step-level timeout
		implementation:       "activity.PowerControl",
		description:          "Execute power control operation (on/off/restart)",
		validateParams:       nil, // No custom validation
	},
	ActionVerifyPowerStatus: {
		requiredParams:       []string{ParamExpectedStatus},
		optionalParams:       []string{},
		requiresPollInterval: true,
		requiresTimeout:      true,
		implementation:       "activity.VerifyPowerStatus",
		description:          "Poll until all components reach expected power status",
		validateParams:       validateVerifyPowerStatusParams,
	},
	ActionVerifyReachability: {
		requiredParams:       []string{ParamComponentTypes},
		optionalParams:       []string{},
		requiresPollInterval: true,
		requiresTimeout:      true,
		implementation:       "activity.VerifyReachability",
		description:          "Poll until specified component types become reachable",
		validateParams:       validateVerifyReachabilityParams,
	},
	ActionGetPowerStatus: {
		requiredParams:       []string{},
		optionalParams:       []string{},
		requiresPollInterval: false,
		requiresTimeout:      true,
		implementation:       "activity.GetPowerStatus",
		description:          "Get current power status of components",
		validateParams:       nil, // No custom validation
	},
	ActionFirmwareControl: {
		requiredParams:       []string{},
		optionalParams:       []string{ParamOperation, ParamPollInterval, ParamPollTimeout},
		requiresPollInterval: false,
		requiresTimeout:      false,
		implementation:       "activity.FirmwareControl + activity.GetFirmwareStatus (async start + poll)",
		description:          "Start firmware update and poll for completion (upgrade/downgrade)",
		validateParams:       nil,
	},
	ActionDecommissionControl: {
		requiredParams:       []string{},
		optionalParams:       []string{},
		requiresPollInterval: false,
		requiresTimeout:      false,
		implementation:       "activity.DecommissionControl",
		description:          "Initiate decommission of target components",
		validateParams:       nil,
	},
	ActionWaitDecommissioned: {
		requiredParams:       []string{},
		optionalParams:       []string{},
		requiresPollInterval: true,
		requiresTimeout:      true,
		implementation:       "activity.GetDecommissionStatus (poll loop)",
		description:          "Poll until all components reach the Decommissioned state",
		validateParams:       nil,
	},
}

// Validate validates an action configuration
func (ac *ActionConfig) Validate() error {
	spec, ok := actionRegistry[ac.Name]
	if !ok {
		return fmt.Errorf("unknown action: %s", ac.Name)
	}

	// Check required parameters
	for _, param := range spec.requiredParams {
		if _, ok := ac.Parameters[param]; !ok {
			return fmt.Errorf(
				"action %s missing required parameter: %s",
				ac.Name,
				param,
			)
		}
	}

	// Check timeout requirement
	if spec.requiresTimeout && ac.Timeout == 0 {
		return fmt.Errorf("action %s requires timeout", ac.Name)
	}

	// Check poll interval requirement
	if spec.requiresPollInterval && ac.PollInterval == 0 {
		return fmt.Errorf("action %s requires poll_interval", ac.Name)
	}

	// Validate parameter values using registered validator
	if spec.validateParams != nil {
		return spec.validateParams(ac.Parameters)
	}

	return nil
}

// ComponentTypes returns component types declared by an action's parameters.
// Actions without component-type parameters return nil.
func (ac ActionConfig) ComponentTypes() ([]devicetypes.ComponentType, error) {
	types, ok := ac.Parameters[ParamComponentTypes]
	if !ok {
		// Most actions do not target component types through parameters. For
		// actions that declare component_types as required, missing it is a
		// malformed rule and should not be silently treated as no targets.
		spec, ok := actionRegistry[ac.Name]
		if ok && spec.requiresParam(ParamComponentTypes) {
			return nil, fmt.Errorf(
				"action %s missing required parameter: %s",
				ac.Name,
				ParamComponentTypes,
			)
		}
		return nil, nil
	}

	return componentTypesFromParameter(types)
}

func (spec actionSpec) requiresParam(param string) bool {
	for _, required := range spec.requiredParams {
		if required == param {
			return true
		}
	}

	return false
}

// validateSleepParams validates Sleep action parameters
func validateSleepParams(params map[string]any) error {
	duration, ok := params[ParamDuration]
	if !ok {
		return fmt.Errorf("missing required parameter: %s", ParamDuration)
	}

	// Duration can be string, time.Duration, or numeric
	switch duration.(type) {
	case string, float64, int, time.Duration:
		// Valid types
		return nil
	default:
		return fmt.Errorf(
			"invalid duration type: %T (expected string, number, or time.Duration)", //nolint
			duration,
		)
	}
}

// validateVerifyPowerStatusParams validates VerifyPowerStatus params
func validateVerifyPowerStatusParams(params map[string]any) error {
	status, ok := params[ParamExpectedStatus]
	if !ok {
		return fmt.Errorf("missing required parameter: %s", ParamExpectedStatus)
	}

	statusStr, ok := status.(string)
	if !ok {
		return fmt.Errorf(
			"expected_status must be string, got %T",
			status,
		)
	}

	// Validate expected status value
	if statusStr != "on" && statusStr != "off" {
		return fmt.Errorf(
			"expected_status must be 'on' or 'off', got '%s'",
			statusStr,
		)
	}

	return nil
}

// validateVerifyReachabilityParams validates VerifyReachability params
func validateVerifyReachabilityParams(params map[string]any) error {
	types, ok := params[ParamComponentTypes]
	if !ok {
		return fmt.Errorf("missing required parameter: %s", ParamComponentTypes)
	}

	_, err := componentTypesFromParameter(types)
	return err
}

func componentTypesFromParameter(types any) ([]devicetypes.ComponentType, error) {
	componentTypes := make([]devicetypes.ComponentType, 0)

	// Can be []string or []any (from JSON/YAML unmarshaling)
	switch v := types.(type) {
	case []string:
		// Validate each component type
		for _, ct := range v {
			componentType := devicetypes.ComponentTypeFromString(ct)
			if componentType == devicetypes.ComponentTypeUnknown {
				return nil, fmt.Errorf("invalid component type: %s", ct)
			}
			componentTypes = append(componentTypes, componentType)
		}
	case []any:
		// Validate each component type
		for _, item := range v {
			ct, ok := item.(string)
			if !ok {
				return nil, fmt.Errorf("component_types must be string array")
			}
			componentType := devicetypes.ComponentTypeFromString(ct)
			if componentType == devicetypes.ComponentTypeUnknown {
				return nil, fmt.Errorf("invalid component type: %s", ct)
			}
			componentTypes = append(componentTypes, componentType)
		}
	default:
		return nil, fmt.Errorf(
			"component_types must be array, got %T",
			types,
		)
	}

	return componentTypes, nil
}
