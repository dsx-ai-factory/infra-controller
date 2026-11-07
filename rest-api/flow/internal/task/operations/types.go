// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operations

import (
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

type PowerOperation int

const (
	PowerOperationUnknown PowerOperation = iota
	// Power On
	PowerOperationPowerOn
	PowerOperationForcePowerOn
	// Power Off
	PowerOperationPowerOff
	PowerOperationForcePowerOff
	// Restart (OS level)
	PowerOperationRestart
	PowerOperationForceRestart
	// Reset (hardware level)
	PowerOperationWarmReset
	PowerOperationColdReset
)

func (o PowerOperation) String() string {
	switch o {
	case PowerOperationPowerOn:
		return "PowerOn"
	case PowerOperationForcePowerOn:
		return "ForcePowerOn"
	case PowerOperationPowerOff:
		return "PowerOff"
	case PowerOperationForcePowerOff:
		return "ForcePowerOff"
	case PowerOperationRestart:
		return "Restart"
	case PowerOperationForceRestart:
		return "ForceRestart"
	case PowerOperationWarmReset:
		return "WarmReset"
	case PowerOperationColdReset:
		return "ColdReset"
	}

	return "Unknown"
}

// powerOperationCodes maps PowerOperation enums to their operation code strings
var powerOperationCodes = map[PowerOperation]string{
	PowerOperationPowerOn:       taskcommon.OpCodePowerControlPowerOn,
	PowerOperationForcePowerOn:  taskcommon.OpCodePowerControlForcePowerOn,
	PowerOperationPowerOff:      taskcommon.OpCodePowerControlPowerOff,
	PowerOperationForcePowerOff: taskcommon.OpCodePowerControlForcePowerOff,
	PowerOperationRestart:       taskcommon.OpCodePowerControlRestart,
	PowerOperationForceRestart:  taskcommon.OpCodePowerControlForceRestart,
	PowerOperationWarmReset:     taskcommon.OpCodePowerControlWarmReset,
	PowerOperationColdReset:     taskcommon.OpCodePowerControlColdReset,
}

// CodeString returns the operation code string for a PowerOperation
func (o PowerOperation) CodeString() string {
	if code, ok := powerOperationCodes[o]; ok {
		return code
	}
	return taskcommon.OpCodePowerControlPowerOn // Default fallback
}

// PowerOperationFromString returns the PowerOperation for a given operation
// code string (e.g. "power_on", "force_power_off"). Returns
// PowerOperationUnknown if the code is not recognized.
func PowerOperationFromString(code string) PowerOperation {
	for op, c := range powerOperationCodes {
		if c == code {
			return op
		}
	}
	return PowerOperationUnknown
}

type PowerStatus string

const (
	PowerStatusUnknown   PowerStatus = "Unknown"
	PowerStatusOn        PowerStatus = "On"
	PowerStatusOff       PowerStatus = "Off"
	PowerStatusRebooting PowerStatus = "Rebooting"
)

type FirmwareOperation int

const (
	FirmwareOperationUnknown FirmwareOperation = iota
	FirmwareOperationUpgrade
	FirmwareOperationDowngrade
	FirmwareOperationRollback
	FirmwareOperationVersion
)

func (o FirmwareOperation) String() string {
	switch o {
	case FirmwareOperationUpgrade:
		return "FirmwareUpgrade"
	case FirmwareOperationDowngrade:
		return "FirmwareDowngrade"
	case FirmwareOperationRollback:
		return "FirmwareRollback"
	case FirmwareOperationVersion:
		return "FirmwareVersion"
	}

	return "Unknown"
}

// firmwareOperationCodes maps FirmwareOperation enums to their operation code strings
var firmwareOperationCodes = map[FirmwareOperation]string{
	FirmwareOperationUpgrade:   taskcommon.OpCodeFirmwareControlUpgrade,
	FirmwareOperationDowngrade: taskcommon.OpCodeFirmwareControlDowngrade,
	FirmwareOperationRollback:  taskcommon.OpCodeFirmwareControlRollback,
}

// MachineBringUpState represents the bring-up state of a
// machine in relation to the power-on gate.
type MachineBringUpState int

const (
	MachineBringUpStateNotDiscovered MachineBringUpState = iota
	MachineBringUpStateWaitingForIngestion
	MachineBringUpStateMachineNotCreated
	MachineBringUpStateMachineCreated
)

func (s MachineBringUpState) String() string {
	switch s {
	case MachineBringUpStateNotDiscovered:
		return "NotDiscovered"
	case MachineBringUpStateWaitingForIngestion:
		return "WaitingForIngestion"
	case MachineBringUpStateMachineNotCreated:
		return "IngestionMachineNotCreated"
	case MachineBringUpStateMachineCreated:
		return "IngestionMachineCreated"
	default:
		return "Unknown"
	}
}

// IsBroughtUp returns true if the machine has passed the
// gate and been ingested.
func (s MachineBringUpState) IsBroughtUp() bool {
	return s == MachineBringUpStateMachineCreated
}

// CodeString returns the operation code string for a FirmwareOperation
func (o FirmwareOperation) CodeString() string {
	if code, ok := firmwareOperationCodes[o]; ok {
		return code
	}
	return taskcommon.OpCodeFirmwareControlUpgrade // Default fallback
}
