// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// hardcodedRuleMap contains pre-built default rules, initialized once at startup
var hardcodedRuleMap map[string]*OperationRule

func init() {
	// Build all hardcoded rules once at startup
	powerOnRule := buildPowerOnRule()
	forcePowerOnRule := buildForcePowerOnRule()
	powerOffRule := buildPowerOffRule()
	forcePowerOffRule := buildForcePowerOffRule()
	restartRule := buildRestartRule()
	forceRestartRule := buildForceRestartRule()
	coldResetRule := buildColdResetRule()
	firmwareUpgradeRule := buildFirmwareUpgradeRule()
	bringUpRule := buildBringUpRule()
	ingestRule := buildIngestRule()
	decommissionRule := buildDecommissionRule()

	// Populate lookup map
	hardcodedRuleMap = map[string]*OperationRule{
		ruleKey(common.TaskTypePowerControl, SequencePowerOn):       powerOnRule,
		ruleKey(common.TaskTypePowerControl, SequenceForcePowerOn):  forcePowerOnRule,
		ruleKey(common.TaskTypePowerControl, SequencePowerOff):      powerOffRule,
		ruleKey(common.TaskTypePowerControl, SequenceForcePowerOff): forcePowerOffRule,
		ruleKey(common.TaskTypePowerControl, SequenceRestart):       restartRule,
		ruleKey(common.TaskTypePowerControl, SequenceForceRestart):  forceRestartRule,
		ruleKey(common.TaskTypePowerControl, SequenceColdReset):     coldResetRule,
		ruleKey(common.TaskTypeFirmwareControl, SequenceUpgrade):    firmwareUpgradeRule,
		ruleKey(common.TaskTypeFirmwareControl, SequenceDowngrade):  firmwareUpgradeRule, // Same rule
		ruleKey(common.TaskTypeFirmwareControl, SequenceRollback):   firmwareUpgradeRule, // Same rule
		ruleKey(common.TaskTypeBringUp, SequenceBringUp):            bringUpRule,
		ruleKey(common.TaskTypeBringUp, SequenceIngest):             ingestRule,
		ruleKey(common.TaskTypeDecommission, SequenceDecommission):  decommissionRule,
	}
}

// buildColdResetRule creates the hardcoded default rule for an AC power cycle.
// Only compute trays support this operation; NVSwitch and PowerShelf managers
// reject ColdReset.
func buildColdResetRule() *OperationRule {
	return &OperationRule{
		Name:          "Hardcoded Default AC Power Cycle",
		Description:   "AC power cycle compute trays",
		OperationType: common.TaskTypePowerControl,
		OperationCode: SequenceColdReset,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps: []SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
				},
			},
		},
	}
}

// ruleKey generates a lookup key for the hardcoded rule map
func ruleKey(operationType common.TaskType, operation string) string {
	return string(operationType) + ":" + operation
}

// buildPowerOnRule creates the hardcoded default rule for power on operations.
// Order: PowerShelf → NVSwitch → Compute.
func buildPowerOnRule() *OperationRule {
	return &OperationRule{
		Name:          "Hardcoded Default Power On",
		Description:   "Fallback rule when no other rule is available",
		OperationType: common.TaskTypePowerControl,
		OperationCode: SequencePowerOn,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps: []SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         1,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      5 * time.Minute,
							PollInterval: 10 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "on",
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         2,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      5 * time.Minute,
							PollInterval: 10 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "on",
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         3,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    1 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      5 * time.Minute,
							PollInterval: 10 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "on",
							},
						},
					},
				},
			},
		},
	}
}

// buildPowerOffRule creates the hardcoded default rule for power off operations.
// Order: Compute → NVSwitch → PowerShelf.
func buildPowerOffRule() *OperationRule {
	return &OperationRule{
		Name:          "Hardcoded Default Power Off",
		Description:   "Fallback rule when no other rule is available",
		OperationType: common.TaskTypePowerControl,
		OperationCode: SequencePowerOff,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps: []SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    1 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      5 * time.Minute,
							PollInterval: 10 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "off",
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         2,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      5 * time.Minute,
							PollInterval: 10 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "off",
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         3,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      5 * time.Minute,
							PollInterval: 10 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "off",
							},
						},
					},
				},
			},
		},
	}
}

// buildRestartRule creates the hardcoded default rule for graceful restart.
// Each stage explicitly specifies the power operation to avoid inheriting the
// composite "restart" operation from the task context (which would send
// BMC GRACEFUL_RESTART — an atomic off→on — instead of separate off/on).
// Off order: Compute → NVSwitch → PowerShelf.
// On order:  PowerShelf → NVSwitch → Compute.
func buildRestartRule() *OperationRule {
	return &OperationRule{
		Name:          "Hardcoded Default Restart",
		Description:   "Composite rule: graceful power off all components then power on",
		OperationType: common.TaskTypePowerControl,
		OperationCode: SequenceRestart,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps: []SequenceStep{
				// === Power Off Sequence (Stages 1-3) ===
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    1 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "power_off",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      10 * time.Minute,
							PollInterval: 15 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "off",
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         2,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "power_off",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      10 * time.Minute,
							PollInterval: 15 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "off",
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         3,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "power_off",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      10 * time.Minute,
							PollInterval: 15 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "off",
							},
						},
					},
				},
				// === Power On Sequence (Stages 4-6) ===
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         4,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "power_on",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      10 * time.Minute,
							PollInterval: 15 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "on",
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         5,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "power_on",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      10 * time.Minute,
							PollInterval: 15 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "on",
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         6,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    1 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "power_on",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      10 * time.Minute,
							PollInterval: 15 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "on",
							},
						},
					},
				},
			},
		},
	}
}

// buildFirmwareUpgradeRule creates the hardcoded default rule for firmware
// operations. PowerShelf is excluded — managed out-of-band.
//
// Power recycle (to activate flashed firmware) is intentionally not part of
// this default. Callers that need an AC cycle after the update can either
// submit a separate power-recycle task or register a custom rule in the
// operation_rules table.
//
//	Stage 1: Compute firmware update
//	Stage 2: NVSwitch firmware update
func buildFirmwareUpgradeRule() *OperationRule {
	return &OperationRule{
		Name:          "Hardcoded Default Firmware Upgrade",
		Description:   "Fallback rule when no other rule is available",
		OperationType: common.TaskTypeFirmwareControl,
		OperationCode: SequenceUpgrade,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps: []SequenceStep{
				// === Stage 1: Compute firmware update ===
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   0,
					Timeout:       45 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    30 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name: ActionFirmwareControl,
						Parameters: map[string]any{
							ParamPollInterval: "2m",
							ParamPollTimeout:  "45m",
						},
					},
				},
				// === Stage 2: NVSwitch firmware update ===
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         2,
					MaxParallel:   0,
					Timeout:       45 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    30 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name: ActionFirmwareControl,
						Parameters: map[string]any{
							ParamPollInterval: "2m",
							ParamPollTimeout:  "45m",
						},
					},
				},
			},
		},
	}
}

// buildForcePowerOnRule creates the hardcoded default rule for
// forced power on operations (no per-step verification).
// Order: PowerShelf → NVSwitch → Compute, then parallel verify all.
func buildForcePowerOnRule() *OperationRule {
	return &OperationRule{
		Name:          "Hardcoded Default Force Power On",
		Description:   "Fallback rule for forced power on (no verification)",
		OperationType: common.TaskTypePowerControl,
		OperationCode: SequenceForcePowerOn,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps: []SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         1,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         2,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         3,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    1 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
					PostOperation: []ActionConfig{
						{
							Name: ActionSleep,
							Parameters: map[string]any{
								ParamDuration: 10 * time.Second,
							},
						},
					},
				},
				// === Final Verification Stage (Stage 4) ===
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         4,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 5 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "on",
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         4, // Parallel with PowerShelf
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 5 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "on",
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         4, // Parallel with PowerShelf and NVSwitch
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 5 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "on",
						},
					},
				},
			},
		},
	}
}

// buildForcePowerOffRule creates the hardcoded default rule for
// forced power off operations (no per-step verification).
// Order: Compute → NVSwitch → PowerShelf, then parallel verify all.
func buildForcePowerOffRule() *OperationRule {
	return &OperationRule{
		Name:          "Hardcoded Default Force Power Off",
		Description:   "Fallback rule for forced power off (no verification)",
		OperationType: common.TaskTypePowerControl,
		OperationCode: SequenceForcePowerOff,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps: []SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    1 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
					PostOperation: []ActionConfig{
						{
							Name: ActionSleep,
							Parameters: map[string]any{
								ParamDuration: 10 * time.Second,
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         2,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
					PostOperation: []ActionConfig{
						{
							Name: ActionSleep,
							Parameters: map[string]any{
								ParamDuration: 5 * time.Second,
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         3,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
					},
					PostOperation: []ActionConfig{
						{
							Name: ActionSleep,
							Parameters: map[string]any{
								ParamDuration: 5 * time.Second,
							},
						},
					},
				},
				// === Final Verification Stage (Stage 4) ===
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         4,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 5 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "off",
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         4, // Parallel with PowerShelf
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 5 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "off",
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         4, // Parallel with PowerShelf and NVSwitch
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 5 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "off",
						},
					},
				},
			},
		},
	}
}

// buildBringUpRule creates the hardcoded default rule for rack bring-up.
//
// Stage 1: PowerShelf — power on, verify power on
// Stage 2: Compute    — power on (bring-up gate), verify power on
// Stage 3: Compute    — firmware check vs desired, trigger update + poll if needed
// Stage 4: NVSwitch  — power on (bring-up), verify power on
// Stage 5: NVSwitch  — verify firmware consistency (no firmware update)
// Stage 6: Compute    — restart (power cycle for firmware activation)
func buildBringUpRule() *OperationRule {
	return &OperationRule{
		Name:          "Hardcoded Default Bring-Up",
		Description:   "Full bring-up: power on, compute firmware update, restart",
		OperationType: common.TaskTypeBringUp,
		OperationCode: SequenceBringUp,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps: []SequenceStep{
				// === Stage 1: PowerShelf — power on, verify ===
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         1,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    10 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "power_on",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      10 * time.Minute,
							PollInterval: 15 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "on",
							},
						},
					},
				},
				// === Stage 2: Compute — power on, verify ===
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         2,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    10 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "power_on",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      10 * time.Minute,
							PollInterval: 15 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "on",
							},
						},
					},
				},
				// === Stage 3: Compute — firmware update (auto-resolve desired) ===
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         3,
					MaxParallel:   0,
					Timeout:       60 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    30 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionFirmwareControl,
						Parameters: map[string]any{
							ParamPollInterval: "2m",
							ParamPollTimeout:  "45m",
						},
					},
				},
				// === Stage 4: NVSwitch — power on, verify ===
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         4,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    10 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "power_on",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      10 * time.Minute,
							PollInterval: 15 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "on",
							},
						},
					},
				},
				// === Stage 5: NVSwitch — verify firmware consistency only ===
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         5,
					MaxParallel:   0,
					Timeout:       10 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    10 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionVerifyFirmwareConsistency,
					},
				},
				// === Stage 6: Compute — restart (power cycle for firmware activation) ===
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         6,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    10 * time.Second,
						BackoffCoefficient: 2.0,
					},
					PreOperation: []ActionConfig{
						{
							Name: ActionPowerControl,
							Parameters: map[string]any{
								ParamOperation: "power_off",
							},
						},
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      5 * time.Minute,
							PollInterval: 15 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "off",
							},
						},
						{
							Name: ActionSleep,
							Parameters: map[string]any{
								ParamDuration: "30s",
							},
						},
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "power_on",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionVerifyPowerStatus,
							Timeout:      10 * time.Minute,
							PollInterval: 15 * time.Second,
							Parameters: map[string]any{
								ParamExpectedStatus: "on",
							},
						},
					},
				},
			},
		},
	}
}

// buildIngestRule creates the default rule for ingestion-only operations.
// PowerShelf is excluded — managed out-of-band.
// This rule registers expected components with their respective component
// manager services without performing power or firmware operations. All component types
// are ingested in parallel within a single stage.
func buildIngestRule() *OperationRule {
	return &OperationRule{
		Name:          "Hardcoded Default Ingestion",
		Description:   "Ingestion-only: register components with component manager services",
		OperationType: common.TaskTypeBringUp,
		OperationCode: SequenceIngest,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps: []SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   0,
					Timeout:       10 * time.Minute,
					MainOperation: ActionConfig{
						Name: ActionInjectExpectation,
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         1, // Parallel with Compute
					MaxParallel:   0,
					Timeout:       10 * time.Minute,
					MainOperation: ActionConfig{
						Name: ActionInjectExpectation,
					},
				},
			},
		},
	}
}

// buildForceRestartRule creates the hardcoded default rule for forced restart.
// Skips per-stage verification for speed but verifies the "off"
// state before proceeding to power on, ensuring a real power cycle occurs.
// Off order:  Compute → NVSwitch → PowerShelf.
// On order:   PowerShelf → NVSwitch → Compute.
func buildForceRestartRule() *OperationRule {
	return &OperationRule{
		Name:          "Hardcoded Default Force Restart",
		Description:   "Forced restart: power off, verify off, then power on",
		OperationType: common.TaskTypePowerControl,
		OperationCode: SequenceForceRestart,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps: []SequenceStep{
				// === Power Off Sequence (Stages 1-3) ===
				// Explicit force_power_off to avoid sending BMC FORCE_RESTART
				// (which is an atomic off→on cycle, not just off).
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    1 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "force_power_off",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name: ActionSleep,
							Parameters: map[string]any{
								ParamDuration: 10 * time.Second,
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         2,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "force_power_off",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name: ActionSleep,
							Parameters: map[string]any{
								ParamDuration: 5 * time.Second,
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         3,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "force_power_off",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name: ActionSleep,
							Parameters: map[string]any{
								ParamDuration: 5 * time.Second,
							},
						},
					},
				},
				// === Verify Off Stage (Stage 4) ===
				// Confirm all components are actually off before powering
				// back on. Without this, a silent power-off failure would
				// result in a "successful restart" that never power-cycled.
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         4,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 15 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "off",
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         4, // Parallel with PowerShelf
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 15 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "off",
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         4, // Parallel with PowerShelf and NVSwitch
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 15 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "off",
						},
					},
				},
				// === Power On Sequence (Stages 5-7) ===
				// Explicit force_power_on to match the force semantics.
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         5,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "force_power_on",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name: ActionSleep,
							Parameters: map[string]any{
								ParamDuration: 10 * time.Second,
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         6,
					MaxParallel:   0,
					Timeout:       15 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "force_power_on",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name: ActionSleep,
							Parameters: map[string]any{
								ParamDuration: 15 * time.Second,
							},
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         7,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    1 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionPowerControl,
						Parameters: map[string]any{
							ParamOperation: "force_power_on",
						},
					},
					PostOperation: []ActionConfig{
						{
							Name: ActionSleep,
							Parameters: map[string]any{
								ParamDuration: 10 * time.Second,
							},
						},
					},
				},
				// === Final Verification Stage (Stage 8) ===
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         8,
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 5 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "on",
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         8, // Parallel with PowerShelf
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 5 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "on",
						},
					},
				},
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         8, // Parallel with PowerShelf and NVSwitch
					MaxParallel:   0,
					Timeout:       20 * time.Minute,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        2,
						InitialInterval:    5 * time.Second,
						BackoffCoefficient: 1.5,
					},
					MainOperation: ActionConfig{
						Name:         ActionVerifyPowerStatus,
						Timeout:      5 * time.Minute,
						PollInterval: 5 * time.Second,
						Parameters: map[string]any{
							ParamExpectedStatus: "on",
						},
					},
				},
			},
		},
	}
}

// buildDecommissionRule creates the hardcoded default rule for rack decommissioning.
//
// Strict ordering is enforced to prevent state corruption:
//
//	Stage 1: Compute — initiate decommission and wait for all to reach Decommissioned
//	Stage 2: NVSwitch — initiate decommission and wait
//	Stage 3: PowerShelf — initiate decommission and wait
func buildDecommissionRule() *OperationRule {
	return &OperationRule{
		Name:          "Hardcoded Default Decommission",
		Description:   "Rack decommission: compute first, then NVSwitch, then PowerShelf",
		OperationType: common.TaskTypeDecommission,
		OperationCode: SequenceDecommission,
		RuleDefinition: RuleDefinition{
			Version: CurrentRuleDefinitionVersion,
			Steps: []SequenceStep{
				// === Stage 1: Compute ===
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   0,
					Timeout:       4 * time.Hour,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    30 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionDecommissionControl,
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionWaitDecommissioned,
							Timeout:      4 * time.Hour,
							PollInterval: 30 * time.Second,
						},
					},
				},
				// === Stage 2: NVSwitch ===
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         2,
					MaxParallel:   0,
					Timeout:       4 * time.Hour,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    30 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionDecommissionControl,
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionWaitDecommissioned,
							Timeout:      4 * time.Hour,
							PollInterval: 30 * time.Second,
						},
					},
				},
				// === Stage 3: PowerShelf ===
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         3,
					MaxParallel:   0,
					Timeout:       4 * time.Hour,
					RetryPolicy: &RetryPolicy{
						MaxAttempts:        3,
						InitialInterval:    30 * time.Second,
						BackoffCoefficient: 2.0,
					},
					MainOperation: ActionConfig{
						Name: ActionDecommissionControl,
					},
					PostOperation: []ActionConfig{
						{
							Name:         ActionWaitDecommissioned,
							Timeout:      4 * time.Hour,
							PollInterval: 30 * time.Second,
						},
					},
				},
			},
		},
	}
}
