// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package task

import (
	"slices"
	"time"

	"github.com/google/uuid"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// PowerTaskDetails contains stored power intent and timestamps for tray and
// rack power details. Attributes retain selected Flow UUIDs even when
// inventory no longer resolves them. The store sorts and deduplicates UUIDs
// within each type; no reports or free-form descriptions are included.
type PowerTaskDetails struct {
	ID         uuid.UUID
	RackID     uuid.UUID
	Operation  operations.PowerControlTaskInfo
	Attributes taskcommon.TaskAttributes
	Status     taskcommon.TaskStatus
	CreatedAt  time.Time
	UpdatedAt  time.Time
	StartedAt  *time.Time
}

// PowerTaskRelevance distinguishes a selected target from a tray affected by
// the task's rack-wide power effects. It does not change scheduling policy.
type PowerTaskRelevance int

const (
	// PowerTaskUnrelated means the task neither selects nor affects this tray.
	PowerTaskUnrelated PowerTaskRelevance = iota
	// PowerTaskSelected means the recorded UUID and component type both match.
	PowerTaskSelected
	// PowerTaskRackAffected means a shelf operation can affect this tray even
	// though the tray was not selected as a target.
	PowerTaskRackAffected
)

// RelevanceFor describes the task's relationship to a tray in the same rack.
// The caller must establish rack membership and a supported tray type
// (Compute, NVSwitch, or PowerShelf) before calling. An empty target list never
// implies that every tray was selected.
func (t PowerTaskDetails) RelevanceFor(
	componentID uuid.UUID,
	componentType devicetypes.ComponentType,
) PowerTaskRelevance {
	if slices.Contains(t.Attributes.ComponentsByType[componentType], componentID) {
		return PowerTaskSelected
	}

	// The shelf rule in conflict.builtinRule applies without component
	// overlap: every shelf power operation can affect other trays in the rack.
	// That effect does not make those trays recorded targets of this task.
	if len(t.Attributes.ComponentsByType[devicetypes.ComponentTypePowerShelf]) > 0 {
		return PowerTaskRackAffected
	}

	return PowerTaskUnrelated
}

// TaskContextMetadata contains only the fields needed to describe nearby work
// and apply the rack conflict policy. Operation payloads and reports are absent.
type TaskContextMetadata struct {
	ID         uuid.UUID
	RackID     uuid.UUID
	Type       taskcommon.TaskType
	Attributes taskcommon.TaskAttributes
	Status     taskcommon.TaskStatus
	CreatedAt  time.Time
}
