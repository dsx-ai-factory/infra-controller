// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package conflict provides data-driven task conflict detection for Flow.
//
// The core abstraction is Rule, a declarative struct that defines which
// operation pairs cannot coexist and at what scope.
// Rule.Conflicts() evaluates the rule against a set of active tasks.
//
// Conflict semantics are intrinsic to what operations do to shared hardware
// and are therefore defined in code, not configurable by users. The built-in
// rule (builtinRule) captures these semantics as explicit operation pairs.
//
// Convention: any new TaskType or ComponentType added to the codebase MUST be
// assessed and the appropriate pairs added to builtinRule in the same PR.
package conflict

import (
	"context"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/google/uuid"
)

// builtinRule is the code-defined selective conflict policy for Flow.
// Only explicitly listed operation pairs conflict; everything else may coexist.
//
// All active tasks passed to Conflicts() are already pre-filtered to the same
// rack by ListActiveTasksForRack, so rack-level scoping is implicit.
// RequireComponentOverlap is only needed when operations are isolated to their
// targeted components and parallel execution on disjoint sets is safe.
//
// ComponentType assignment rationale:
//   - PowerShelf power ops have RequireComponentOverlap=false (rack-level):
//     cutting power to a shelf affects all components regardless of UUIDs.
//   - Compute / NVSwitch power and firmware ops use
//     RequireComponentOverlap=true: isolated to their targeted components.
//   - BringUp has RequireComponentOverlap=false (rack-level): comprehensive.
//   - Decommission has RequireComponentOverlap=false (rack-level): rack-
//     destructive; exclusive with every other rack operation including itself.
//   - ComponentTypeUnknown (zero value) acts as a wildcard — matches any
//     component type including tasks with nil ComponentsByType (old tasks).
//
// IMPORTANT: when adding a new TaskType or ComponentType, update this table
// in the same PR after assessing which operations it cannot coexist with.
var builtinRule = &Rule{ //nolint
	ConflictingPairs: []Entry{
		// PowerShelf power ops block all power ops on the rack: cutting
		// power to a shelf affects every component.
		{
			A: OperationSpec{
				OperationType: string(taskcommon.TaskTypePowerControl),
				ComponentType: devicetypes.ComponentTypePowerShelf,
			},
			B: OperationSpec{
				OperationType: string(taskcommon.TaskTypePowerControl),
			},
		},
		// PowerShelf power ops block all firmware upgrades on the rack:
		// unsafe to flash any component while shelf power is in flux.
		{
			A: OperationSpec{
				OperationType: string(taskcommon.TaskTypePowerControl),
				ComponentType: devicetypes.ComponentTypePowerShelf,
			},
			B: OperationSpec{
				OperationType: string(
					taskcommon.TaskTypeFirmwareControl),
			},
		},
		// Compute power ops block each other on overlapping components.
		{
			A: OperationSpec{
				OperationType: string(taskcommon.TaskTypePowerControl),
				ComponentType: devicetypes.ComponentTypeCompute,
			},
			B: OperationSpec{
				OperationType: string(taskcommon.TaskTypePowerControl),
				ComponentType: devicetypes.ComponentTypeCompute,
			},
			RequireComponentOverlap: true,
		},
		// NVSwitch power ops block each other on overlapping components.
		{
			A: OperationSpec{
				OperationType: string(taskcommon.TaskTypePowerControl),
				ComponentType: devicetypes.ComponentTypeNVSwitch,
			},
			B: OperationSpec{
				OperationType: string(taskcommon.TaskTypePowerControl),
				ComponentType: devicetypes.ComponentTypeNVSwitch,
			},
			RequireComponentOverlap: true,
		},
		// Compute power ops block firmware upgrades on overlapping
		// compute components.
		{
			A: OperationSpec{
				OperationType: string(taskcommon.TaskTypePowerControl),
				ComponentType: devicetypes.ComponentTypeCompute,
			},
			B: OperationSpec{
				OperationType: string(
					taskcommon.TaskTypeFirmwareControl),
				ComponentType: devicetypes.ComponentTypeCompute,
			},
			RequireComponentOverlap: true,
		},
		// NVSwitch power ops block firmware upgrades on overlapping
		// NVSwitch components.
		{
			A: OperationSpec{
				OperationType: string(taskcommon.TaskTypePowerControl),
				ComponentType: devicetypes.ComponentTypeNVSwitch,
			},
			B: OperationSpec{
				OperationType: string(
					taskcommon.TaskTypeFirmwareControl),
				ComponentType: devicetypes.ComponentTypeNVSwitch,
			},
			RequireComponentOverlap: true,
		},
		// Firmware upgrades block each other on overlapping components
		// regardless of component type: flashing the same hardware
		// concurrently is unsafe.
		{
			A: OperationSpec{
				OperationType: string(
					taskcommon.TaskTypeFirmwareControl),
			},
			B: OperationSpec{
				OperationType: string(
					taskcommon.TaskTypeFirmwareControl),
			},
			RequireComponentOverlap: true,
		},
		// BringUp is comprehensive and blocks all other operations.
		{
			A: OperationSpec{
				OperationType: string(taskcommon.TaskTypeBringUp),
			},
			B: OperationSpec{
				OperationType: "*",
			},
		},
		// Decommission is rack-destructive and exclusive with allocation,
		// power, firmware, bring-up, maintenance, and another decommission
		// (see docs/design/decommissioning/rack-decommissioning.md). Flow
		// has no separate allocation/maintenance TaskType today, so the
		// wildcard covers every current and future rack task type.
		{
			A: OperationSpec{
				OperationType: string(taskcommon.TaskTypeDecommission),
			},
			B: OperationSpec{
				OperationType: "*",
			},
		},
	},
}

// OperationSpec matches an operation by type, code, and component type.
// The wildcard value "*" matches any string field; ComponentTypeUnknown (0)
// matches any component type (including tasks with nil ComponentsByType).
type OperationSpec struct {
	OperationType string                    // e.g. "power_control", "*"
	OperationCode string                    // e.g. "power_on", "*"; "" = wildcard
	ComponentType devicetypes.ComponentType // ComponentTypeUnknown = wildcard
}

// Matches returns true if this spec matches the given operation type and code.
// ComponentType is NOT checked here — it is evaluated separately against the
// task's Attributes.ComponentsByType in Rule.Conflicts().
func (s OperationSpec) Matches(op OperationSpec) bool {
	typeMatch := s.OperationType == "*" || s.OperationType == op.OperationType
	codeMatch := s.OperationCode == "" ||
		s.OperationCode == "*" ||
		s.OperationCode == op.OperationCode
	return typeMatch && codeMatch
}

// Entry is a symmetric pair of operations that cannot coexist when the scope
// condition is satisfied.
type Entry struct {
	A OperationSpec
	B OperationSpec

	// RequireComponentOverlap narrows conflict detection to task pairs
	// that share at least one component UUID. When false, any two
	// matching tasks on the same rack conflict (rack pre-filtering by
	// ListActiveTasksForRack makes an explicit rack check unnecessary).
	RequireComponentOverlap bool
}

// opMatch reports whether e matches the (a, b) operation pair directionally:
// entry.A matches a and entry.B matches b on type and code.
func (e Entry) opMatch(a, b OperationSpec) bool {
	return e.A.Matches(a) && e.B.Matches(b)
}

// componentMatch reports whether e's component-type constraints are satisfied
// directionally: entry.A's component type is present in taskA and entry.B's
// component type is present in taskB.
func (e Entry) componentMatch(taskA, taskB *taskdef.Task) bool {
	return hasComponentType(taskA, e.A.ComponentType) &&
		hasComponentType(taskB, e.B.ComponentType)
}

// Rule declaratively defines when operations conflict.
// Empty ConflictingPairs means exclusive mode: any active task is a conflict.
type Rule struct {
	// ConflictingPairs lists operation pairs that cannot coexist within
	// the scope. Each entry is evaluated symmetrically. Empty = all
	// active operations conflict (exclusive mode).
	ConflictingPairs []Entry

	// AtomicAcrossRacks: when true, conflict checking spans all racks in
	// the same task group — the entire multi-rack operation is rejected or
	// queued as a unit if any rack has a conflict. For V3 in the future,
	// currently always false.
	AtomicAcrossRacks bool
}

// Conflicts returns true if the incoming task conflicts with any of the active
// tasks under this rule. A conflict requires:
//  1. The operation pair matches a ConflictingPairs entry (type, code, and
//     component type all match for the respective tasks).
//  2. If RequireComponentOverlap is true, the two tasks must share at least
//     one component UUID.
//
// All tasks in activeTasks are assumed to be on the same rack as incoming
// (pre-filtered by ListActiveTasksForRack).
//
// Special case: empty ConflictingPairs is exclusive mode — any active task
// is a conflict.
func (r *Rule) Conflicts(
	incoming *taskdef.Task,
	activeTasks []*taskdef.Task,
) bool {
	if len(r.ConflictingPairs) == 0 {
		return len(activeTasks) > 0
	}

	incomingOp := OperationSpec{
		OperationType: string(incoming.Operation.Type),
		OperationCode: incoming.Operation.Code,
	}

	for _, active := range activeTasks {
		activeOp := OperationSpec{
			OperationType: string(active.Operation.Type),
			OperationCode: active.Operation.Code,
		}
		for _, entry := range r.ConflictingPairs {
			if !entry.opMatch(incomingOp, activeOp) && !entry.opMatch(activeOp, incomingOp) {
				continue
			}

			// Apply component-type checks in the matched direction.
			// Both directions may hold when A and B match the same op type.
			componentMatch :=
				(entry.opMatch(incomingOp, activeOp) && entry.componentMatch(incoming, active)) ||
					(entry.opMatch(activeOp, incomingOp) && entry.componentMatch(active, incoming))

			if componentMatch &&
				(!entry.RequireComponentOverlap ||
					componentUUIDsOverlap(incoming, active)) {
				return true
			}
		}
	}

	return false
}

// componentUUIDsOverlap returns true when tasks a and b share at least one
// component UUID across all component types.
func componentUUIDsOverlap(a, b *taskdef.Task) bool {
	uuidsA := a.Attributes.AllComponentUUIDs()
	if len(uuidsA) == 0 {
		return false
	}

	setA := make(map[uuid.UUID]struct{}, len(uuidsA))
	for _, id := range uuidsA {
		setA[id] = struct{}{}
	}

	for _, id := range b.Attributes.AllComponentUUIDs() {
		if _, ok := setA[id]; ok {
			return true
		}
	}

	return false
}

// hasComponentType returns true if task t includes at least one component of
// the given type. ComponentTypeUnknown in the entry spec is a wildcard —
// it matches any task regardless of component type.
func hasComponentType(
	t *taskdef.Task,
	ct devicetypes.ComponentType,
) bool {
	if ct == devicetypes.ComponentTypeUnknown {
		return true // entry wildcard
	}
	return len(t.Attributes.ComponentsByType[ct]) > 0
}

// Resolver determines whether an incoming operation conflicts with active
// tasks on a rack using the code-defined builtinRule.
type Resolver struct {
	store taskstore.Store
}

// NewResolver creates a new Resolver backed by the given store.
func NewResolver(store taskstore.Store) *Resolver {
	return &Resolver{store: store}
}

// HasConflict returns true if the incoming task would conflict with any
// existing active task on the same rack under the builtin conflict rule.
func (r *Resolver) HasConflict(
	ctx context.Context,
	incoming *taskdef.Task,
) (bool, error) {
	activeTasks, err := r.store.ListActiveTasksForRack(
		ctx, incoming.RackID,
	)
	if err != nil {
		return false, err
	}

	return builtinRule.Conflicts(incoming, activeTasks), nil
}

// HasScheduleConflict reports whether the incoming operation would conflict
// with any of the existing schedule operations.
//
// This is a coarse-grained advisory check: only operation type and code are
// matched against the ConflictingPairs table. Component-type checks and
// component-UUID overlap checks are intentionally skipped because the exact
// components are not known at schedule creation time (they are resolved from
// scope rows at fire time). The check is therefore conservative — it may
// return true when a runtime check would not — but it will never miss a
// conflict that would be caught at runtime.
//
// For precise per-component conflict detection use HasConflict, which requires
// full task Attributes (populated from live inventory).
func (r *Resolver) HasScheduleConflict(
	incoming operation.Wrapper,
	existing []operation.Wrapper,
) bool {
	incomingOp := OperationSpec{
		OperationType: string(incoming.Type),
		OperationCode: incoming.Code,
	}

	for _, op := range existing {
		activeOp := OperationSpec{
			OperationType: string(op.Type),
			OperationCode: op.Code,
		}
		for _, entry := range builtinRule.ConflictingPairs {
			if entry.opMatch(incomingOp, activeOp) || entry.opMatch(activeOp, incomingOp) {
				return true
			}
		}
	}

	return false
}
