// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package capabilityrequirements

import (
	"cmp"
	"errors"
	"fmt"
	"slices"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// Requirement is a component-manager capability needed before a task can be
// dispatched.
type Requirement struct {
	ComponentType devicetypes.ComponentType
	Capabilities  capability.CapabilitySet
}

// CapabilityChecker verifies that a component type supports a required
// component-manager capability.
type CapabilityChecker interface {
	CheckCapability(devicetypes.ComponentType, capability.Capability) error
}

// Validate checks every capability in r against checker.
func (r Requirement) Validate(checker CapabilityChecker) error {
	if checker == nil {
		return errors.New("capability checker is nil")
	}

	for _, capability := range r.Capabilities {
		err := checker.CheckCapability(r.ComponentType, capability)
		if err != nil {
			return fmt.Errorf(
				"component type %s requires capability %q: %w",
				devicetypes.ComponentTypeToString(r.ComponentType),
				capability,
				err,
			)
		}
	}

	return nil
}

func newRequirement(
	componentType devicetypes.ComponentType,
	capabilities ...capability.Capability,
) (Requirement, error) {
	capabilitySet, err := capability.NewSet(capabilities...)
	if err != nil {
		return Requirement{}, err
	}

	return Requirement{
		ComponentType: componentType,
		Capabilities:  capabilitySet,
	}, nil
}

// requirementBuilder collects capability requirements while preserving the
// distinction between component types mentioned by a rack rule and component
// types actually targeted by the task.
type requirementBuilder struct {
	targetTypes  map[devicetypes.ComponentType]struct{}
	requirements map[devicetypes.ComponentType]capability.CapabilitySet
}

// actionCapabilities maps rule action names to the component-manager
// capabilities they require.
var actionCapabilities = map[string]capability.CapabilitySet{
	operationrules.ActionSleep: nil,
	operationrules.ActionPowerControl: {
		capability.CapabilityPowerControl,
	},
	operationrules.ActionVerifyPowerStatus: {
		capability.CapabilityPowerStatus,
	},
	operationrules.ActionGetPowerStatus: {
		capability.CapabilityPowerStatus,
	},
	operationrules.ActionFirmwareControl: {
		capability.CapabilityFirmwareControl,
		capability.CapabilityFirmwareStatus,
	},
	operationrules.ActionBringUpControl: {
		capability.CapabilityBringUpControl,
	},
	operationrules.ActionWaitBringUp: {
		capability.CapabilityBringUpStatus,
	},
	operationrules.ActionInjectExpectation: {
		capability.CapabilityInjectExpectation,
	},
	operationrules.ActionVerifyFirmwareConsistency: {
		capability.CapabilityFirmwareConsistencyCheck,
	},
	operationrules.ActionVerifyReachability: {
		capability.CapabilityPowerStatus,
	},
}

// Required derives the component-manager capabilities needed by target
// component types. The rule describes actions available for a rack, while
// componentTypes limits validation to the component types actually targeted by
// the task.
func Required(
	rule *operationrules.RuleDefinition,
	componentTypes []devicetypes.ComponentType,
) ([]Requirement, error) {
	if rule == nil {
		return nil, nil
	}

	builder := newRequirementBuilder(componentTypes)

	for _, step := range rule.Steps {
		if !builder.isTargetedType(step.ComponentType) {
			continue
		}

		for _, action := range step.OrderedActions() {
			err := builder.collectAction(step.ComponentType, action)
			if err != nil {
				return nil, err
			}
		}
	}

	return builder.build()
}

// newRequirementBuilder records the component types explicitly targeted by the
// task, so rule actions that mention other rack components do not add
// requirements for managers this task will never call.
func newRequirementBuilder(
	componentTypes []devicetypes.ComponentType,
) *requirementBuilder {
	types := make(map[devicetypes.ComponentType]struct{}, len(componentTypes))
	for _, componentType := range componentTypes {
		types[componentType] = struct{}{}
	}

	return &requirementBuilder{
		targetTypes:  types,
		requirements: make(map[devicetypes.ComponentType]capability.CapabilitySet),
	}
}

// isTargetedType reports whether the task execution request includes the
// component type.
func (b *requirementBuilder) isTargetedType(typ devicetypes.ComponentType) bool {
	_, ok := b.targetTypes[typ]
	return ok
}

// add records a required capability for a component type and de-duplicates
// repeated requirements from multiple rule actions.
func (b *requirementBuilder) add(
	componentType devicetypes.ComponentType,
	capability capability.Capability,
) error {
	capabilities, err := b.requirements[componentType].Add(capability)
	if err != nil {
		return err
	}

	b.requirements[componentType] = capabilities
	return nil
}

// collectAction applies the action's capability set to component types declared
// by the action parameters plus the action's step component type.
func (b *requirementBuilder) collectAction(
	componentType devicetypes.ComponentType,
	action operationrules.ActionConfig,
) error {
	capabilities, ok := actionCapabilities[action.Name]
	if !ok {
		return fmt.Errorf("unknown action %q in capability validation", action.Name)
	}

	// Some actions, such as VerifyReachability, declare additional component
	// types in their parameters.
	componentTypes, err := action.ComponentTypes()
	if err != nil {
		return err
	}

	// Every action also applies to the component type of its rule step.
	componentTypes = append(componentTypes, componentType)

	for _, componentType := range componentTypes {
		if !b.isTargetedType(componentType) {
			// Skip actions that do not target this task's component types.
			continue
		}

		for _, capability := range capabilities {
			if err := b.add(componentType, capability); err != nil {
				return err
			}
		}
	}

	return nil
}

// build returns a deterministic list of capability requirements.
func (b *requirementBuilder) build() ([]Requirement, error) {
	result := make([]Requirement, 0, len(b.requirements))
	for componentType, capabilities := range b.requirements {
		requirement, err := newRequirement(componentType, capabilities...)
		if err != nil {
			return nil, err
		}

		result = append(
			result,
			requirement,
		)
	}

	slices.SortFunc(
		result,
		func(a, b Requirement) int {
			return cmp.Compare(a.ComponentType, b.ComponentType)
		},
	)

	return result, nil
}
