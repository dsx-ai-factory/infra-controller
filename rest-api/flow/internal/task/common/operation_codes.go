// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import "fmt"

// OperationCode identifies an operation within a TaskType. Codes are only
// meaningful together with their task type.
type OperationCode string

// Inject-expectation operation codes.
const OpCodeInjectExpectation = "inject_expectation"

// Power control operation codes.
//
// force_power_on and warm_reset have no distinct Flow RPC or REST action.
// Public client aliases normalize them to PowerOnRack and PowerResetRack,
// respectively. cold_reset is exposed as the REST acpowercycle action through
// ACPowerCycleRack.
const (
	OpCodePowerControlPowerOn       = "power_on"
	OpCodePowerControlForcePowerOn  = "force_power_on"
	OpCodePowerControlPowerOff      = "power_off"
	OpCodePowerControlForcePowerOff = "force_power_off"
	OpCodePowerControlRestart       = "restart"
	OpCodePowerControlForceRestart  = "force_restart"
	OpCodePowerControlWarmReset     = "warm_reset"
	OpCodePowerControlColdReset     = "cold_reset"
)

// Firmware control operation codes
const (
	OpCodeFirmwareControlUpgrade   = "upgrade"
	OpCodeFirmwareControlDowngrade = "downgrade"
	OpCodeFirmwareControlRollback  = "rollback"
)

// Bring-up operation codes
const (
	OpCodeBringUp = "bring_up"
	OpCodeIngest  = "ingest"
)

// Decommission operation codes
const (
	OpCodeDecommission = "decommission"
)

// String returns the operation code's wire value.
func (c OperationCode) String() string {
	return string(c)
}

// ValidateFor checks that the operation code belongs to the given task type.
func (c OperationCode) ValidateFor(taskType TaskType) error {
	switch taskType {
	case TaskTypeInjectExpectation:
		if c == OpCodeInjectExpectation {
			return nil
		}
	case TaskTypePowerControl:
		switch c {
		case OpCodePowerControlPowerOn,
			OpCodePowerControlForcePowerOn,
			OpCodePowerControlPowerOff,
			OpCodePowerControlForcePowerOff,
			OpCodePowerControlRestart,
			OpCodePowerControlForceRestart,
			OpCodePowerControlWarmReset,
			OpCodePowerControlColdReset:
			return nil
		}
	case TaskTypeFirmwareControl:
		switch c {
		case OpCodeFirmwareControlUpgrade,
			OpCodeFirmwareControlDowngrade,
			OpCodeFirmwareControlRollback:
			return nil
		}
	case TaskTypeBringUp:
		if c == OpCodeBringUp || c == OpCodeIngest {
			return nil
		}
	case TaskTypeDecommission:
		if c == OpCodeDecommission {
			return nil
		}
	default:
		return fmt.Errorf("task type %q is invalid", taskType)
	}

	return fmt.Errorf("operation code %q is invalid for task type %q", c, taskType)
}
