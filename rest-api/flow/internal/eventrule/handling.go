// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"time"

	"github.com/google/uuid"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

// HandlingTask identifies one Task created while handling an event.
type HandlingTask struct {
	ID          uuid.UUID
	ExecutionID uuid.UUID
	Status      taskcommon.TaskStatus
	CreatedAt   time.Time
}

// HandlingExecution describes dispatch progress independently of created Tasks.
// Only fields needed to compute the overall handling result are projected.
type HandlingExecution struct {
	ID         uuid.UUID
	ActionName string
	Status     ExecutionStatus
	Reason     ExecutionReason
}

// Handling is one application of an event rule and its associated work.
type Handling struct {
	ID              uuid.UUID
	EventType       Type
	EventSummary    string
	FirstObservedAt time.Time
	AppliedPolicy   Policy
	ResourceID      string
	ResourceType    ResourceKind
	Executions      []HandlingExecution
	Tasks           []HandlingTask
	HandledAt       time.Time
}
