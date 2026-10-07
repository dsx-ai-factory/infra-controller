// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"fmt"
	"slices"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

// Power control sequence names - use shared operation codes
const (
	SequencePowerOn       = common.OpCodePowerControlPowerOn
	SequenceForcePowerOn  = common.OpCodePowerControlForcePowerOn
	SequencePowerOff      = common.OpCodePowerControlPowerOff
	SequenceForcePowerOff = common.OpCodePowerControlForcePowerOff
	SequenceRestart       = common.OpCodePowerControlRestart
	SequenceForceRestart  = common.OpCodePowerControlForceRestart
	SequenceWarmReset     = common.OpCodePowerControlWarmReset
	SequenceColdReset     = common.OpCodePowerControlColdReset
)

// Firmware control sequence names - use shared operation codes
const (
	SequenceUpgrade   = common.OpCodeFirmwareControlUpgrade
	SequenceDowngrade = common.OpCodeFirmwareControlDowngrade
	SequenceRollback  = common.OpCodeFirmwareControlRollback
)

// Bring-up sequence names - use shared operation codes
const (
	SequenceBringUp = common.OpCodeBringUp
	SequenceIngest  = common.OpCodeIngest
)

// Decommission sequence names - use shared operation codes
const (
	SequenceDecommission = common.OpCodeDecommission
)

// RequiredOperations maps operation types to their required operations
// These operations must have rules defined for the system to function properly
var RequiredOperations = map[common.TaskType][]string{
	common.TaskTypePowerControl: {
		SequencePowerOn,
		SequencePowerOff,
	},
	common.TaskTypeFirmwareControl: {
		SequenceUpgrade,
	},
}

// ValidOperations maps operation types to their valid operation codes
// Used for validating operation names in YAML configuration
var ValidOperations = map[common.TaskType][]string{
	common.TaskTypePowerControl: {
		SequencePowerOn,
		SequenceForcePowerOn,
		SequencePowerOff,
		SequenceForcePowerOff,
		SequenceRestart,
		SequenceForceRestart,
		SequenceWarmReset,
		SequenceColdReset,
	},
	common.TaskTypeFirmwareControl: {
		SequenceUpgrade,
		SequenceDowngrade,
		SequenceRollback,
	},
	common.TaskTypeBringUp: {
		SequenceBringUp,
		SequenceIngest,
	},
	common.TaskTypeDecommission: {
		SequenceDecommission,
	},
}

// IsValidOperation checks if an operation code is valid for the given operation type
// Returns true if the operation is recognized, false otherwise
func IsValidOperation(opType common.TaskType, operation string) bool {
	if operation == "" {
		return false
	}

	validOps, ok := ValidOperations[opType]
	if !ok {
		// No valid operations defined for this type
		return false
	}

	return slices.Contains(validOps, operation)
}

// ValidateRequiredOperations checks if all required operations have rules defined
// Takes a collection of rules for an operation type and verifies completeness
func ValidateRequiredOperations(
	opType common.TaskType,
	rules map[string]*OperationRule,
) error {
	requiredOps, ok := RequiredOperations[opType]
	if !ok {
		// No required operations defined for this operation type
		return nil
	}

	for _, opName := range requiredOps {
		if _, exists := rules[opName]; !exists {
			return fmt.Errorf(
				"required operation '%s' missing for operation type %s",
				opName,
				opType,
			)
		}
	}

	return nil
}

// ValidateStageSequence validates that stages form a valid sequence
// Stages should start from 1 and increment, though gaps are allowed
func ValidateStageSequence(steps []SequenceStep) error {
	if len(steps) == 0 {
		return nil
	}

	minStage := steps[0].Stage
	for _, step := range steps {
		if step.Stage < minStage {
			minStage = step.Stage
		}
	}

	if minStage < 1 {
		return fmt.Errorf("stages must start from 1 or higher, found stage %d", minStage)
	}

	return nil
}
