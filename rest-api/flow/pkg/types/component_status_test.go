// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestComponentOperationStatus_IsReady(t *testing.T) {
	assert.True(t, ComponentOperationStatus{Phase: PhaseReady}.IsReady())
	assert.False(t, ComponentOperationStatus{Phase: PhaseInUse}.IsReady())
	assert.False(t, ComponentOperationStatus{
		Phase:             PhaseReady,
		BlockedOperations: []OperationType{OperationTypePowerControl},
	}.IsReady())
}

func TestComponentOperationStatus_Blocks(t *testing.T) {
	s := ComponentOperationStatus{BlockedOperations: []OperationType{OperationTypeFirmwareControl}}
	assert.True(t, s.Blocks(OperationTypeFirmwareControl))
	assert.False(t, s.Blocks(OperationTypePowerControl))
}

func TestComponentOperationStatus_Equal(t *testing.T) {
	base := ComponentOperationStatus{
		Phase:             PhaseInUse,
		Reason:            "Assigned/Provisioning",
		BlockedOperations: []OperationType{OperationTypePowerControl, OperationTypeFirmwareControl},
	}
	same := ComponentOperationStatus{
		Phase:             PhaseInUse,
		Reason:            "Assigned/Provisioning",
		BlockedOperations: []OperationType{OperationTypePowerControl, OperationTypeFirmwareControl},
	}
	diffPhase := ComponentOperationStatus{Phase: PhaseReady, Reason: base.Reason, BlockedOperations: base.BlockedOperations}
	diffReason := ComponentOperationStatus{Phase: base.Phase, Reason: "other", BlockedOperations: base.BlockedOperations}
	diffOpsLen := ComponentOperationStatus{Phase: base.Phase, Reason: base.Reason, BlockedOperations: []OperationType{OperationTypePowerControl}}
	diffOpsOrder := ComponentOperationStatus{Phase: base.Phase, Reason: base.Reason, BlockedOperations: []OperationType{OperationTypeFirmwareControl, OperationTypePowerControl}}

	assert.True(t, base.Equal(same))
	assert.False(t, base.Equal(diffPhase))
	assert.False(t, base.Equal(diffReason))
	assert.False(t, base.Equal(diffOpsLen))
	assert.False(t, base.Equal(diffOpsOrder))
}

func TestAggregateComponentOperationStatus(t *testing.T) {
	status := func(phase Phase) *ComponentOperationStatus {
		return &ComponentOperationStatus{Phase: phase}
	}

	tests := []struct {
		name     string
		statuses []*ComponentOperationStatus
		want     Phase
	}{
		{name: "all ready", statuses: []*ComponentOperationStatus{status(PhaseReady), status(PhaseReady)}, want: PhaseReady},
		{name: "in use precedes ready", statuses: []*ComponentOperationStatus{status(PhaseReady), status(PhaseInUse)}, want: PhaseInUse},
		{name: "deleting precedes in use", statuses: []*ComponentOperationStatus{status(PhaseInUse), status(PhaseDeleting)}, want: PhaseDeleting},
		{name: "initializing precedes deleting", statuses: []*ComponentOperationStatus{status(PhaseDeleting), status(PhaseInitializing)}, want: PhaseInitializing},
		{name: "error precedes initializing", statuses: []*ComponentOperationStatus{status(PhaseInitializing), status(PhaseError)}, want: PhaseError},
		{name: "unknown precedes error", statuses: []*ComponentOperationStatus{status(PhaseError), status(PhaseUnknown)}, want: PhaseUnknown},
		{name: "missing status is unknown", statuses: []*ComponentOperationStatus{status(PhaseError), nil}, want: PhaseUnknown},
		{name: "unrecognized phase is unknown", statuses: []*ComponentOperationStatus{status(Phase("STALE")), status(PhaseError)}, want: PhaseUnknown},
		{name: "empty rack is unknown", want: PhaseUnknown},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, AggregateComponentOperationStatus(tt.statuses))
		})
	}
}
