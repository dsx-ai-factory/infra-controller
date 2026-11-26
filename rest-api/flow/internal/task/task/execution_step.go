// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package task

import (
	"encoding/json"
	"fmt"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/firmwareversion"
)

// ExecutionStep binds an action sequence to its execution targets and options.
type ExecutionStep struct {
	Definition operationrules.SequenceStep
	Components []WorkflowComponent
	// OperationInfo overrides the task payload, using the same OperationType.
	// When absent, the original task payload is used unchanged. Firmware options
	// must apply to these targets; supply an override for a narrower selection.
	OperationInfo json.RawMessage `json:",omitempty"`
}

// ResolveOperationInfo decodes and validates the step's effective task options.
func (s ExecutionStep) ResolveOperationInfo(info ExecutionInfo) (operations.Operation, error) {
	raw := s.OperationInfo
	if len(raw) == 0 {
		raw = info.OperationInfo
	}
	op, err := operations.New(info.OperationType, raw)
	if err != nil {
		return nil, err
	}
	err = op.Validate()
	return op, err
}

// ValidateAdditionalSteps checks stage ordering, target scope and options before
// any additional operation is dispatched. Main rule validation remains separate.
func (info ExecutionInfo) ValidateAdditionalSteps() error {
	if len(info.AdditionalSteps) == 0 {
		return nil
	}
	switch info.OperationType {
	case taskcommon.TaskTypeFirmwareControl, taskcommon.TaskTypePowerControl,
		taskcommon.TaskTypeBringUp, taskcommon.TaskTypeDecommission:
	default:
		return fmt.Errorf("task type %s does not execute additional rule steps", info.OperationType)
	}
	if info.RuleDefinition == nil {
		return fmt.Errorf("additional steps require a main rule")
	}
	mainStage := info.RuleDefinition.GetMaxStage()
	lastStage := mainStage
	seen := make(map[int]map[devicetypes.ComponentType]bool)
	for i, step := range info.AdditionalSteps {
		def := step.Definition
		err := def.Validate()
		if err != nil {
			return fmt.Errorf("additional step %d: %w", i, err)
		}
		if def.Stage <= mainStage || def.Stage < lastStage {
			return fmt.Errorf("additional step %d must be ordered after main stage %d", i, mainStage)
		}
		lastStage = def.Stage
		if seen[def.Stage] == nil {
			seen[def.Stage] = make(map[devicetypes.ComponentType]bool)
		}
		if seen[def.Stage][def.ComponentType] {
			return fmt.Errorf("additional step %d duplicates component type in stage %d", i, def.Stage)
		}
		seen[def.Stage][def.ComponentType] = true
		if len(step.Components) == 0 {
			return fmt.Errorf("additional step %d has no targets", i)
		}
		usesMAC := false
		for _, component := range step.Components {
			if component.Type != def.ComponentType || (component.ComponentID == "" && component.MACAddress == "") {
				return fmt.Errorf("additional step %d has an invalid target for %s", i, def.ComponentType)
			}
			usesMAC = usesMAC || component.ComponentID == ""
		}
		// The target builder uses one addressing mode for the entire step.
		if usesMAC {
			for _, component := range step.Components {
				if component.MACAddress == "" {
					return fmt.Errorf("additional step %d requires a MAC address for every target in a mixed batch", i)
				}
			}
		}
		options, err := step.ResolveOperationInfo(info)
		if err != nil {
			return fmt.Errorf("additional step %d options: %w", i, err)
		}
		firmware, firmwareOptions := options.(*operations.FirmwareControlTaskInfo)
		if firmwareOptions {
			for _, action := range def.OrderedActions() {
				if action.Name == operationrules.ActionFirmwareControl {
					_, selected := firmwareversion.Extract(firmware.TargetVersion, def.ComponentType)
					if !selected {
						return fmt.Errorf("additional step %d firmware version excludes %s targets", i, def.ComponentType)
					}
				}
			}
		}

	}
	return nil
}
