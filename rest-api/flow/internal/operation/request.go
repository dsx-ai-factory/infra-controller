// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operation

import (
	"bytes"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

// Wrapper wraps the operation type and its serialized information.
type Wrapper struct {
	Type taskcommon.TaskType
	Code string          // Operation code string (e.g., "power_on", "upgrade")
	Info json.RawMessage // Serialized operation details
}

// Clone returns an independent copy of the operation wrapper and its serialized
// information.
func (w Wrapper) Clone() Wrapper {
	cloned := w
	cloned.Info = bytes.Clone(w.Info)
	return cloned
}

// ConflictStrategy controls how a task behaves when a conflict is detected.
type ConflictStrategy int

const (
	// ConflictStrategyReject immediately rejects the task when a conflict is detected (default).
	ConflictStrategyReject ConflictStrategy = iota
	// ConflictStrategyQueue queues the task until the conflicting task completes.
	ConflictStrategyQueue
)

// String returns the stable name of the conflict strategy.
func (s ConflictStrategy) String() string {
	switch s {
	case ConflictStrategyReject:
		return "reject"
	case ConflictStrategyQueue:
		return "queue"
	default:
		return fmt.Sprintf("ConflictStrategy(%d)", s)
	}
}

// ParseConflictStrategy parses a conflict strategy's stable name.
func ParseConflictStrategy(value string) (ConflictStrategy, error) {
	switch value {
	case "reject":
		return ConflictStrategyReject, nil
	case "queue":
		return ConflictStrategyQueue, nil
	default:
		return ConflictStrategyReject, fmt.Errorf(
			"unknown conflict strategy %q",
			value,
		)
	}
}

// Request represents the specification of an operation submitted by the user.
// The Task Manager resolves the TargetSpec, splits by rack, and creates one
// Task per rack.
type Request struct {
	Operation   Wrapper
	TargetSpec  TargetSpec // Exactly one of racks, NVLink domains, or components
	Description string

	// ConflictStrategy controls how the task behaves when a conflict is
	// detected. Default (ConflictStrategyReject) rejects on conflict.
	ConflictStrategy ConflictStrategy

	// QueueTimeout is how long to wait in queue before auto-expiry. Zero
	// means use the server default. The server may enforce a maximum.
	// Only relevant when ConflictStrategy is ConflictStrategyQueue.
	QueueTimeout time.Duration

	// Optional: override rule resolution with a specific rule
	RuleID *uuid.UUID

	// RequiredRackID, when non-zero, causes SubmitTask to return an error
	// (and create no tasks) if the resolved targets do not belong exclusively
	// to this rack. Use this for component-targeting requests where the scope
	// was originally written against a specific rack, to guard against the
	// case where all listed components have since been moved to a different
	// single rack or span multiple racks.
	//
	// This field only handles the single-rack enforcement case. If a future
	// caller needs to constrain resolution to a known set of multiple racks,
	// this would need to become []uuid.UUID (or a separate AllowedRackIDs
	// field). Do not add that generalization until there is a concrete caller.
	RequiredRackID uuid.UUID

	// IdempotencyKey, when set, makes submission create or return one task for
	// this request. It is only valid for requests constrained to one rack.
	IdempotencyKey string

	// TriggerType and TriggerID identify the upstream resource that caused this
	// request. Each supported trigger type defines whether an occurrence ID is
	// required or prohibited.
	TriggerType TriggerType
	TriggerID   *uuid.UUID
}

func (r *Request) HasIdempotencyKey() bool {
	return r != nil && r.IdempotencyKey != ""
}

func (r *Request) Validate() error {
	if r == nil {
		return fmt.Errorf("request is nil")
	}

	if !r.Operation.Type.IsValid() {
		return fmt.Errorf("unknown task type")
	}

	if err := r.TargetSpec.Validate(); err != nil {
		return fmt.Errorf("invalid target spec: %w", err)
	}

	if r.HasIdempotencyKey() && r.RequiredRackID == uuid.Nil {
		return fmt.Errorf("idempotency key requires RequiredRackID")
	}

	if err := ValidateTrigger(r.TriggerType, r.TriggerID); err != nil {
		return err
	}

	return nil
}
