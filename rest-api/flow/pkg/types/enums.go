// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package types provides public domain types for the Flow client.
// This package has minimal dependencies (only uuid) and can be imported
// by external modules for interface definitions and mocking without
// pulling in gRPC dependencies.
package types

import "fmt"

// ComponentType represents the type of a rack component.
type ComponentType string

const (
	ComponentTypeUnknown    ComponentType = "UNKNOWN"
	ComponentTypeCompute    ComponentType = "COMPUTE"
	ComponentTypeNVSwitch   ComponentType = "NVSWITCH"
	ComponentTypePowerShelf ComponentType = "POWERSHELF"
	ComponentTypeTORSwitch  ComponentType = "TORSWITCH"
	ComponentTypeUMS        ComponentType = "UMS"
	ComponentTypeCDU        ComponentType = "CDU"
)

// ParseComponentType converts a string into a validated ComponentType.
func ParseComponentType(value string) (ComponentType, error) {
	componentType := ComponentType(value)
	if err := componentType.Validate(); err != nil {
		return ComponentTypeUnknown, err
	}

	return componentType, nil
}

// Validate checks that the component type identifies a concrete supported type.
func (ct ComponentType) Validate() error {
	switch ct {
	case ComponentTypeCompute,
		ComponentTypeNVSwitch,
		ComponentTypePowerShelf,
		ComponentTypeTORSwitch,
		ComponentTypeUMS,
		ComponentTypeCDU:
		return nil
	default:
		return fmt.Errorf("unknown component type %q", ct)
	}
}

// BMCType represents the type of BMC (Baseboard Management Controller).
type BMCType string

const (
	BMCTypeUnknown BMCType = "UNKNOWN"
	BMCTypeHost    BMCType = "HOST"
	BMCTypeDPU     BMCType = "DPU"
)

// PowerControlOp represents a power control operation.
type PowerControlOp string

const (
	PowerControlOpOn           PowerControlOp = "ON"
	PowerControlOpForceOn      PowerControlOp = "FORCE_ON"
	PowerControlOpOff          PowerControlOp = "OFF"
	PowerControlOpForceOff     PowerControlOp = "FORCE_OFF"
	PowerControlOpRestart      PowerControlOp = "RESTART"
	PowerControlOpForceRestart PowerControlOp = "FORCE_RESTART"
	PowerControlOpWarmReset    PowerControlOp = "WARM_RESET"
	PowerControlOpColdReset    PowerControlOp = "COLD_RESET"
)

// TaskStatus represents the status of an async task.
type TaskStatus string

const (
	TaskStatusUnknown   TaskStatus = "UNKNOWN"
	TaskStatusPending   TaskStatus = "PENDING"
	TaskStatusRunning   TaskStatus = "RUNNING"
	TaskStatusCompleted TaskStatus = "COMPLETED"
	TaskStatusFailed    TaskStatus = "FAILED"
)

// TaskExecutorType represents the type of task executor.
type TaskExecutorType string

const (
	TaskExecutorTypeUnknown  TaskExecutorType = "UNKNOWN"
	TaskExecutorTypeTemporal TaskExecutorType = "TEMPORAL"
)

// DiffType represents the type of difference in component validation.
type DiffType string

const (
	DiffTypeUnknown    DiffType = "Unknown"
	DiffTypeMissing    DiffType = "Missing"
	DiffTypeUnexpected DiffType = "Unexpected"
	DiffTypeMismatch   DiffType = "Mismatch"
)

// OperationType represents the type of operation (power control, firmware, etc.).
type OperationType string

const (
	OperationTypeUnknown         OperationType = "UNKNOWN"
	OperationTypePowerControl    OperationType = "POWER_CONTROL"
	OperationTypeFirmwareControl OperationType = "FIRMWARE_CONTROL"
)

// LeakStatus is Flow's view of whether coolant leak detection has fired
// for a component. It is set by the leak-detection loop from core's
// tray-leak-detection health alert; LeakStatusUnknown is the resting value
// for components the loop has not yet evaluated.
type LeakStatus string

const (
	// LeakStatusUnknown: the leak-detection loop has not evaluated this
	// component yet (e.g. before the first cycle, or a type the loop does
	// not cover).
	LeakStatusUnknown LeakStatus = "UNKNOWN"
	// LeakStatusDetected: core is reporting an active leak alert.
	LeakStatusDetected LeakStatus = "DETECTED"
	// LeakStatusNotDetected: the loop evaluated the component and core is
	// not reporting a leak alert.
	LeakStatusNotDetected LeakStatus = "NOT_DETECTED"
)

// Phase is Flow's coarse operability bucket. Component phases are shared across
// compute, nvswitch, and power shelf; map new Core sub-states onto an existing
// phase rather than adding new ones. Rack operation status aggregates these
// phases according to AggregateComponentOperationStatus.
type Phase string

const (
	// PhaseUnknown: no (or undecodable) core state observed yet.
	PhaseUnknown Phase = "UNKNOWN"
	// PhaseInitializing: on the path to Ready (provisioning, validating).
	PhaseInitializing Phase = "INITIALIZING"
	// PhaseReady: steady operational state.
	PhaseReady Phase = "READY"
	// PhaseInUse: tenant-owned or core has in-progress work.
	PhaseInUse Phase = "IN_USE"
	// PhaseError: terminal failure; needs human intervention.
	PhaseError Phase = "ERROR"
	// PhaseDeleting: being torn down.
	PhaseDeleting Phase = "DELETING"
)
