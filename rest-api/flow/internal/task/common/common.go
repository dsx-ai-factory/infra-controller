// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/google/uuid"
)

type TaskType string

const (
	TaskTypeUnknown           TaskType = "unknown"
	TaskTypeInjectExpectation TaskType = "inject_expectation"
	TaskTypePowerControl      TaskType = "power_control"
	TaskTypeFirmwareControl   TaskType = "firmware_control"
	TaskTypeBringUp           TaskType = "bring_up"
	TaskTypeDecommission      TaskType = "decommission"
)

func TaskTypeFromString(s string) TaskType {
	switch s {
	case TaskTypeInjectExpectation.String():
		return TaskTypeInjectExpectation
	case TaskTypePowerControl.String():
		return TaskTypePowerControl
	case TaskTypeFirmwareControl.String():
		return TaskTypeFirmwareControl
	case TaskTypeBringUp.String():
		return TaskTypeBringUp
	case TaskTypeDecommission.String():
		return TaskTypeDecommission
	default:
		return TaskTypeUnknown
	}
}

func (tt TaskType) IsZero() bool {
	return tt == ""
}

func (tt TaskType) IsValid() bool {
	return TaskTypeFromString(tt.String()) != TaskTypeUnknown
}

func (tt TaskType) String() string {
	return string(tt)
}

type ExecutorType string

const (
	ExecutorTypeUnknown  ExecutorType = "unknown"
	ExecutorTypeTemporal ExecutorType = "temporal"
)

func (et ExecutorType) IsValid() bool {
	return et != ExecutorTypeUnknown
}

type TaskStatus string

const (
	TaskStatusUnknown    TaskStatus = "unknown"
	TaskStatusPending    TaskStatus = "pending"
	TaskStatusRunning    TaskStatus = "running"
	TaskStatusCompleted  TaskStatus = "completed"
	TaskStatusFailed     TaskStatus = "failed"
	TaskStatusTerminated TaskStatus = "terminated"
	// TaskStatusWaiting means the task is waiting for a pre-execution condition,
	// such as rack availability or target linkage. It is NOT a finished state.
	TaskStatusWaiting TaskStatus = "waiting"
)

func (s TaskStatus) IsFinished() bool {
	return s == TaskStatusCompleted ||
		s == TaskStatusFailed ||
		s == TaskStatusTerminated
}

// NonTerminalTaskStatuses returns every status surfaced by active-only task
// queries and inventory task statistics.
func NonTerminalTaskStatuses() []TaskStatus {
	return []TaskStatus{
		TaskStatusWaiting,
		TaskStatusPending,
		TaskStatusRunning,
	}
}

type TaskListOptions struct {
	TaskType TaskType
	RackID   uuid.UUID
	// ComponentID, when non-zero, restricts results to tasks whose
	// TaskAttributes.ComponentsByType contains this UUID under any
	// component type.
	ComponentID uuid.UUID
	ActiveOnly  bool
}

type OperationRuleListOptions struct {
	OperationType TaskType
	IsDefault     *bool
}

// TaskAttributes holds flexible task metadata stored as a single jsonb
// column. New fields can be added here without requiring a DB migration.
type TaskAttributes struct {
	// ComponentsByType maps each targeted component type to its UUIDs.
	// Nil means the task targets no specific components.
	ComponentsByType map[devicetypes.ComponentType][]uuid.UUID `json:"components_by_type,omitempty"` //nolint:lll
}

// AllComponentUUIDs returns a flat slice of all component UUIDs across every
// component type, in no guaranteed order.
func (a TaskAttributes) AllComponentUUIDs() []uuid.UUID {
	total := 0
	for _, ids := range a.ComponentsByType {
		total += len(ids)
	}

	if total == 0 {
		return nil
	}

	uuids := make([]uuid.UUID, 0, total)
	for _, ids := range a.ComponentsByType {
		uuids = append(uuids, ids...)
	}

	return uuids //nolint:gocritic
}

type ComponentInfo struct {
	Type        devicetypes.ComponentType
	DeviceInfo  deviceinfo.DeviceInfo
	ComponentID string // Component ID from the component manager service
}

func (ci *ComponentInfo) Validate() error {
	if ci.Type == devicetypes.ComponentTypeUnknown {
		return fmt.Errorf("component type is unknown")
	}

	if !ci.DeviceInfo.VerifyIDOrSerial() {
		return fmt.Errorf("component device info is invalid")
	}

	return nil
}
