// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
)

// Activity implementations use these helpers to bridge descriptor-level
// capability validation and Go's operation-specific manager interfaces. The
// registry verifies that the selected manager declares the capability, and the
// typed wrappers verify that the implementation also satisfies the matching
// interface before an operation is dispatched.

// requireCapableManager validates the target, finds a manager that declares
// the required capability, and returns it as the operation interface expected
// by the caller.
func requireCapableManager[T any](
	registry *componentmanager.Registry,
	target common.Target,
	requiredCapability capability.Capability,
) (T, error) {
	var zero T

	err := target.Validate()
	if err != nil {
		return zero, fmt.Errorf("target is invalid: %w", err)
	}

	// Registry owns nil-receiver handling so all manager lookups return the
	// same configuration error when no registry is available.
	cm, err := registry.GetCapableManager(target.Type, requiredCapability)
	if err != nil {
		return zero, err
	}

	// Capability metadata is validated before dispatch; this assertion catches
	// implementation bugs where the descriptor and Go interface drift apart.
	typedManager, ok := cm.(T)
	if !ok {
		descriptor := cm.Descriptor()
		return zero, componentmanager.CapabilityInterfaceNotImplementedError{
			ComponentType:  descriptor.Type,
			Implementation: descriptor.Implementation,
			Capability:     requiredCapability,
		}
	}

	return typedManager, nil
}

// requireExpectationInjector returns the manager interface for expectation
// injection operations.
func requireExpectationInjector(
	registry *componentmanager.Registry,
	target common.Target,
) (componentmanager.ExpectationInjector, error) {
	return requireCapableManager[componentmanager.ExpectationInjector](
		registry,
		target,
		capability.CapabilityInjectExpectation,
	)
}

// requirePowerController returns the manager interface for power control
// operations.
func requirePowerController(
	registry *componentmanager.Registry,
	target common.Target,
) (componentmanager.PowerController, error) {
	return requireCapableManager[componentmanager.PowerController](
		registry,
		target,
		capability.CapabilityPowerControl,
	)
}

// requirePowerStatusReader returns the manager interface for power status
// reads.
func requirePowerStatusReader(
	registry *componentmanager.Registry,
	target common.Target,
) (componentmanager.PowerStatusReader, error) {
	return requireCapableManager[componentmanager.PowerStatusReader](
		registry,
		target,
		capability.CapabilityPowerStatus,
	)
}

// requireFirmwareController returns the manager interface for firmware control
// operations.
func requireFirmwareController(
	registry *componentmanager.Registry,
	target common.Target,
) (componentmanager.FirmwareController, error) {
	return requireCapableManager[componentmanager.FirmwareController](
		registry,
		target,
		capability.CapabilityFirmwareControl,
	)
}

// requireFirmwareStatusReader returns the manager interface for firmware
// status reads.
func requireFirmwareStatusReader(
	registry *componentmanager.Registry,
	target common.Target,
) (componentmanager.FirmwareStatusReader, error) {
	return requireCapableManager[componentmanager.FirmwareStatusReader](
		registry,
		target,
		capability.CapabilityFirmwareStatus,
	)
}

// requireBringUpController returns the manager interface for bring-up control
// operations.
func requireBringUpController(
	registry *componentmanager.Registry,
	target common.Target,
) (componentmanager.BringUpController, error) {
	return requireCapableManager[componentmanager.BringUpController](
		registry,
		target,
		capability.CapabilityBringUpControl,
	)
}

// requireBringUpStatusReader returns the manager interface for bring-up status
// reads.
func requireBringUpStatusReader(
	registry *componentmanager.Registry,
	target common.Target,
) (componentmanager.BringUpStatusReader, error) {
	return requireCapableManager[componentmanager.BringUpStatusReader](
		registry,
		target,
		capability.CapabilityBringUpStatus,
	)
}

// requireDecommissioner returns the manager interface for decommission control
// operations.
func requireDecommissioner(
	registry *componentmanager.Registry,
	target common.Target,
) (componentmanager.Decommissioner, error) {
	return requireCapableManager[componentmanager.Decommissioner](
		registry,
		target,
		capability.CapabilityDecommissionControl,
	)
}

// requireDecommissionStatusReader returns the manager interface for decommission
// status reads.
func requireDecommissionStatusReader(
	registry *componentmanager.Registry,
	target common.Target,
) (componentmanager.DecommissionStatusReader, error) {
	return requireCapableManager[componentmanager.DecommissionStatusReader](
		registry,
		target,
		capability.CapabilityDecommissionStatus,
	)
}

// requireFirmwareConsistencyChecker returns the manager interface for firmware
// consistency checks.
func requireFirmwareConsistencyChecker(
	registry *componentmanager.Registry,
	target common.Target,
) (componentmanager.FirmwareConsistencyChecker, error) {
	return requireCapableManager[componentmanager.FirmwareConsistencyChecker](
		registry,
		target,
		capability.CapabilityFirmwareConsistencyCheck,
	)
}
