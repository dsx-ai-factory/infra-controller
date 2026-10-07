// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
)

// ComponentFilterKind aliases the shared operation target filter kind.
type ComponentFilterKind = operation.ComponentFilterKind

const (
	// ComponentFilterKindTypes filters by component type (e.g. COMPUTE, POWERSHELF).
	ComponentFilterKindTypes = operation.ComponentFilterKindTypes
	// ComponentFilterKindComponents targets specific components by their UUIDs.
	ComponentFilterKindComponents = operation.ComponentFilterKindComponents
)

// ComponentFilter aliases the shared operation target filter shape.
type ComponentFilter = operation.ComponentFilter

// MarshalComponentFilter marshals a ComponentFilter to JSON for JSONB storage.
func MarshalComponentFilter(cf *ComponentFilter) (json.RawMessage, error) {
	return operation.MarshalComponentFilter(cf)
}

// ComponentFilterEqual reports whether two component_filter JSONB values are
// semantically equivalent. nil, empty, and the JSON null literal are all
// treated as equivalent (they all mean "all components"). For type-based
// filters, element order is ignored.
func ComponentFilterEqual(a, b json.RawMessage) (bool, error) {
	return operation.ComponentFilterEqual(a, b)
}

// UnmarshalComponentFilter parses a JSONB value into a ComponentFilter.
// Returns nil if raw is nil, empty, or the JSON null literal — all three
// representations mean "no filter" (target all components in the rack).
// The JSON null case arises when bun's AppendJSONValue serialises a nil
// json.RawMessage for a jsonb-typed column without the nullzero tag.
func UnmarshalComponentFilter(raw json.RawMessage) (*ComponentFilter, error) {
	return operation.UnmarshalComponentFilter(raw)
}

// TaskScheduleScope is the bun model for the task_schedule_scope table.
// Each row represents one rack target in a schedule's scope.
// LastTaskID tracks the task produced for this rack by the most recent firing,
// used by the overlap check to determine whether the previous execution is still active.
//
// Invariant: when ComponentFilter has kind "components", every UUID listed
// in that filter must belong to RackID. This is enforced by the API write
// path (resolveComponentScope groups components by rack before persisting).
// At fire time the dispatcher sets RequiredRackID on the SubmitTask request,
// so any violation caused by a direct DB modification (e.g. a component moved
// to a different rack) is detected before any task is created, and the scope
// is skipped with an error.
type TaskScheduleScope struct {
	bun.BaseModel `bun:"table:task_schedule_scope,alias:tss"`

	ID              uuid.UUID       `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	ScheduleID      uuid.UUID       `bun:"schedule_id,type:uuid,notnull"`
	RackID          uuid.UUID       `bun:"rack_id,type:uuid,notnull"`
	ComponentFilter json.RawMessage `bun:"component_filter,type:jsonb,nullzero"`
	LastTaskID      *uuid.UUID      `bun:"last_task_id,type:uuid"`
	CreatedAt       time.Time       `bun:"created_at,notnull,default:current_timestamp"`
}
