// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

// OperationRun is the bun model for the operation_run table.
type OperationRun struct {
	bun.BaseModel `bun:"table:operation_run,alias:orun"`

	ID                uuid.UUID                             `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	Name              string                                `bun:"name,notnull"`
	Description       string                                `bun:"description,nullzero"`
	Status            operationrun.OperationRunStatus       `bun:"status,type:varchar(32),notnull"`        //nolint:lll
	StatusReason      operationrun.OperationRunStatusReason `bun:"status_reason,type:varchar(64),notnull"` //nolint:lll
	StatusMessage     string                                `bun:"status_message,nullzero"`
	CurrentPhaseIndex int32                                 `bun:"current_phase_index,notnull"`
	TotalPhases       int32                                 `bun:"total_phases,notnull"`
	Selector          json.RawMessage                       `bun:"selector,type:jsonb,notnull"`
	Options           json.RawMessage                       `bun:"options,type:jsonb,notnull"`
	OperationTemplate json.RawMessage                       `bun:"operation_template,type:jsonb,notnull"`
	OperationType     taskcommon.TaskType                   `bun:"operation_type,type:varchar(64),notnull"`
	OperationCode     string                                `bun:"operation_code,type:varchar(64),notnull"`
	CreatedAt         time.Time                             `bun:"created_at,notnull,default:current_timestamp"`
	UpdatedAt         time.Time                             `bun:"updated_at,notnull,default:current_timestamp"`
	StartedAt         *time.Time                            `bun:"started_at"`
	FinishedAt        *time.Time                            `bun:"finished_at"`
}

// OperationRunTarget is the bun model for one rack execution target in an
// operation run. ComponentsByType is the concrete component set resolved by the
// planner, not the caller-facing target filter expression.
type OperationRunTarget struct {
	bun.BaseModel `bun:"table:operation_run_target,alias:ort"`

	ID               uuid.UUID                             `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	OperationRunID   uuid.UUID                             `bun:"operation_run_id,type:uuid,notnull"`
	RackID           uuid.UUID                             `bun:"rack_id,type:uuid,notnull"`
	SequenceIndex    int32                                 `bun:"sequence_index,notnull"`
	PhaseIndex       int32                                 `bun:"phase_index,notnull"`
	ComponentsByType operation.ComponentsByType            `bun:"components_by_type,type:jsonb,notnull"`
	TaskID           *uuid.UUID                            `bun:"task_id,type:uuid"`
	Status           operationrun.OperationRunTargetStatus `bun:"status,type:varchar(32),notnull"` //nolint:lll
	Message          string                                `bun:"message,nullzero"`
	RetryAfter       *time.Time                            `bun:"retry_after"`
	RetryState       json.RawMessage                       `bun:"retry_state,type:jsonb,nullzero"`
	CreatedAt        time.Time                             `bun:"created_at,notnull,default:current_timestamp"`
	UpdatedAt        time.Time                             `bun:"updated_at,notnull,default:current_timestamp"`
}
