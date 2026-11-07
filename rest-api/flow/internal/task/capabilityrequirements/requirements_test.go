// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package capabilityrequirements

import (
	"errors"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestRequired(t *testing.T) {
	rule := &operationrules.RuleDefinition{
		Steps: []operationrules.SequenceStep{
			{
				ComponentType: devicetypes.ComponentTypeCompute,
				PreOperation: []operationrules.ActionConfig{
					{
						Name: operationrules.ActionVerifyReachability,
						Parameters: map[string]any{
							operationrules.ParamComponentTypes: []string{
								"Compute",
								"PowerShelf",
								"NVSwitch",
							},
						},
					},
				},
				MainOperation: operationrules.ActionConfig{
					Name: operationrules.ActionFirmwareControl,
				},
			},
			{
				ComponentType: devicetypes.ComponentTypeNVSwitch,
				MainOperation: operationrules.ActionConfig{
					Name: operationrules.ActionPowerControl,
				},
			},
		},
	}

	requirements, err := Required(rule, []devicetypes.ComponentType{
		devicetypes.ComponentTypeCompute,
		devicetypes.ComponentTypePowerShelf,
	})

	require.NoError(t, err)
	require.Equal(t, []Requirement{
		{
			ComponentType: devicetypes.ComponentTypeCompute,
			Capabilities: capability.CapabilitySet{
				capability.CapabilityFirmwareControl,
				capability.CapabilityFirmwareStatus,
				capability.CapabilityPowerStatus,
			},
		},
		{
			ComponentType: devicetypes.ComponentTypePowerShelf,
			Capabilities: capability.CapabilitySet{
				capability.CapabilityPowerStatus,
			},
		},
	}, requirements)
}

func TestNewRequirementNormalizesCapabilities(t *testing.T) {
	requirement, err := newRequirement(
		devicetypes.ComponentTypeCompute,
		capability.CapabilityPowerControl,
		capability.CapabilityFirmwareControl,
		capability.CapabilityPowerControl,
	)

	require.NoError(t, err)
	require.Equal(t, Requirement{
		ComponentType: devicetypes.ComponentTypeCompute,
		Capabilities: capability.CapabilitySet{
			capability.CapabilityFirmwareControl,
			capability.CapabilityPowerControl,
		},
	}, requirement)
}

func TestRequirementValidate(t *testing.T) {
	t.Run("checks each capability", func(t *testing.T) {
		checker := &recordingCapabilityChecker{}
		req := Requirement{
			ComponentType: devicetypes.ComponentTypeCompute,
			Capabilities: capability.CapabilitySet{
				capability.CapabilityPowerControl,
				capability.CapabilityPowerStatus,
			},
		}

		err := req.Validate(checker)

		require.NoError(t, err)
		require.Equal(t, []capabilityCheck{
			{
				componentType: devicetypes.ComponentTypeCompute,
				capability:    capability.CapabilityPowerControl,
			},
			{
				componentType: devicetypes.ComponentTypeCompute,
				capability:    capability.CapabilityPowerStatus,
			},
		}, checker.calls)
	})

	t.Run("wraps checker error", func(t *testing.T) {
		checkerErr := errors.New("check failed")
		checker := &recordingCapabilityChecker{err: checkerErr}
		req := Requirement{
			ComponentType: devicetypes.ComponentTypeCompute,
			Capabilities: capability.CapabilitySet{
				capability.CapabilityPowerControl,
			},
		}

		err := req.Validate(checker)

		require.ErrorIs(t, err, checkerErr)
		require.ErrorContains(
			t,
			err,
			`component type Compute requires capability "PowerControl"`,
		)
	})

	t.Run("requires checker", func(t *testing.T) {
		req := Requirement{
			ComponentType: devicetypes.ComponentTypeCompute,
			Capabilities: capability.CapabilitySet{
				capability.CapabilityPowerControl,
			},
		}

		err := req.Validate(nil)

		require.ErrorContains(t, err, "capability checker is nil")
	})
}

type capabilityCheck struct {
	componentType devicetypes.ComponentType
	capability    capability.Capability
}

type recordingCapabilityChecker struct {
	calls []capabilityCheck
	err   error
}

func (c *recordingCapabilityChecker) CheckCapability(
	componentType devicetypes.ComponentType,
	capability capability.Capability,
) error {
	c.calls = append(c.calls, capabilityCheck{
		componentType: componentType,
		capability:    capability,
	})

	return c.err
}
