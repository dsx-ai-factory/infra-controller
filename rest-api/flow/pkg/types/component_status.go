// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

// ComponentOperationStatus is Flow's view of a component's operability. It is
// derived from a source-specific state machine (today: Core's per-type
// controller state, mapped in internal/nicoapi) and recomputed on every
// inventory sync. Only the type lives here; mappers belong with the
// source whose raw values they decode.
type ComponentOperationStatus struct {
	Phase             Phase           `json:"phase"`
	Reason            string          `json:"reason,omitempty"`
	BlockedOperations []OperationType `json:"blocked_operations,omitempty"`
}

// AggregateComponentOperationStatus returns the strictest operability phase
// present in statuses. Missing, empty, and unrecognized statuses fail closed as
// Unknown. An empty component set is also Unknown because the rack has no
// evidence that it is ready.
func AggregateComponentOperationStatus(statuses []*ComponentOperationStatus) Phase {
	if len(statuses) == 0 {
		return PhaseUnknown
	}

	result := PhaseReady
	for _, status := range statuses {
		phase := PhaseUnknown
		if status != nil {
			phase = normalizedPhase(status.Phase)
		}

		if phasePriority(phase) > phasePriority(result) {
			result = phase
		}
		if result == PhaseUnknown {
			return result
		}
	}

	return result
}

func normalizedPhase(phase Phase) Phase {
	switch phase {
	case PhaseReady, PhaseInUse, PhaseDeleting, PhaseInitializing, PhaseError, PhaseUnknown:
		return phase
	default:
		return PhaseUnknown
	}
}

func phasePriority(phase Phase) int {
	// Priority represents aggregation severity; higher values win:
	// Unknown > Error > Initializing > Deleting > InUse > Ready.
	switch phase {
	case PhaseReady:
		return 1
	case PhaseInUse:
		return 2
	case PhaseDeleting:
		return 3
	case PhaseInitializing:
		return 4
	case PhaseError:
		return 5
	case PhaseUnknown:
		return 6
	default:
		return 6
	}
}

// IsReady returns true when the component is in Ready phase with no
// blocked operations of interest. It is a convenience for callers that
// only need a boolean go/no-go.
func (s ComponentOperationStatus) IsReady() bool {
	return s.Phase == PhaseReady && len(s.BlockedOperations) == 0
}

// Blocks reports whether op is in BlockedOperations.
func (s ComponentOperationStatus) Blocks(op OperationType) bool {
	for _, b := range s.BlockedOperations {
		if b == op {
			return true
		}
	}
	return false
}

// Equal reports whether two ComponentOperationStatus values are identical. Needed
// because BlockedOperations is a slice and ComponentOperationStatus is therefore
// not comparable with ==.
func (s ComponentOperationStatus) Equal(other ComponentOperationStatus) bool {
	if s.Phase != other.Phase || s.Reason != other.Reason {
		return false
	}
	if len(s.BlockedOperations) != len(other.BlockedOperations) {
		return false
	}
	for i := range s.BlockedOperations {
		if s.BlockedOperations[i] != other.BlockedOperations[i] {
			return false
		}
	}
	return true
}
