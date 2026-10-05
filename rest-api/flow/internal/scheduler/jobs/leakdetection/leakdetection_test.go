// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package leakdetection

import (
	"context"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/operation"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// --- mock taskmanager.Manager ---

type mockManager struct {
	requests       []*operation.Request
	submitErr      error
	cancelErr      error
	returnNoTaskID bool
}

func (m *mockManager) Start(_ context.Context) error { return nil }
func (m *mockManager) Stop(_ context.Context)        {}

func (m *mockManager) SubmitTask(_ context.Context, req *operation.Request) ([]uuid.UUID, error) {
	m.requests = append(m.requests, req)
	if m.submitErr != nil {
		return nil, m.submitErr
	}
	if m.returnNoTaskID {
		return nil, nil
	}
	return []uuid.UUID{uuid.New()}, nil
}

func (m *mockManager) CancelTask(_ context.Context, _ uuid.UUID) error {
	return m.cancelErr
}

// --- tests ---

func TestSubmitPowerOffTask_Success(t *testing.T) {
	ctx := context.Background()
	mgr := &mockManager{}
	machineID := "machine-abc-123"

	err := submitPowerOffTask(ctx, mgr, machineID, devicetypes.ComponentTypeCompute)
	require.NoError(t, err)
	require.Len(t, mgr.requests, 1)

	req := mgr.requests[0]

	// Verify target spec uses component targeting with ExternalRef
	assert.True(t, req.TargetSpec.IsComponentTargeting())
	require.Len(t, req.TargetSpec.Components, 1)

	comp := req.TargetSpec.Components[0]
	assert.Equal(t, uuid.Nil, comp.UUID)
	require.NotNil(t, comp.External)
	assert.Equal(t, devicetypes.ComponentTypeCompute, comp.External.Type)
	assert.Equal(t, machineID, comp.External.ID)

	// Verify conflict strategy is queue
	assert.Equal(t, operation.ConflictStrategyQueue, req.ConflictStrategy)

	// Verify description contains machine ID
	assert.Contains(t, req.Description, machineID)
}

func TestSubmitPowerOffTask_NoTasksCreated(t *testing.T) {
	ctx := context.Background()
	mgr := &mockManager{returnNoTaskID: true}

	err := submitPowerOffTask(ctx, mgr, "machine-xyz", devicetypes.ComponentTypeCompute)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "failed to create any power-off tasks")
}

func TestSubmitPowerOffTask_SubmitError(t *testing.T) {
	ctx := context.Background()
	mgr := &mockManager{submitErr: errors.New("submit failed")}

	err := submitPowerOffTask(ctx, mgr, "machine-xyz", devicetypes.ComponentTypeCompute)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "submit failed")
}

func TestRunLeakDetectionOne_NoLeaks(t *testing.T) {
	ctx := context.Background()
	nicoClient := nicoapi.NewMockClient()
	mgr := &mockManager{}

	runLeakDetectionOne(ctx, nicoClient, mgr, nil)

	assert.Empty(t, mgr.requests)
}

func TestRunLeakDetectionOne_SubmitsTaskPerMachine(t *testing.T) {
	ctx := context.Background()
	mgr := &mockManager{}

	machines := []string{"machine-1", "machine-2", "machine-3"}
	nicoClient := nicoapi.NewMockClient()
	nicoClient.SetLeakingMachineIds(machines)

	runLeakDetectionOne(ctx, nicoClient, mgr, nil)

	require.Len(t, mgr.requests, 3)
	for i, m := range machines {
		assert.Equal(t, m, mgr.requests[i].TargetSpec.Components[0].External.ID)
	}
}

func TestRunLeakDetectionOne_ContinuesOnSubmitError(t *testing.T) {
	ctx := context.Background()
	nicoClient := nicoapi.NewMockClient()
	nicoClient.SetLeakingMachineIds([]string{"machine-a", "machine-b"})
	mgr := &mockManager{submitErr: errors.New("always fails")}

	runLeakDetectionOne(ctx, nicoClient, mgr, nil)

	// Verify all machines were attempted despite errors
	require.Len(t, mgr.requests, 2)
}

func TestRunLeakDetectionOne_SubmitsTaskPerSwitch(t *testing.T) {
	ctx := context.Background()
	mgr := &mockManager{}

	switches := []string{"switch-1", "switch-2", "switch-3"}
	nicoClient := nicoapi.NewMockClient()
	nicoClient.SetLeakingSwitchIds(switches)

	runLeakDetectionOne(ctx, nicoClient, mgr, nil)

	require.Len(t, mgr.requests, 3)
	for i, s := range switches {
		assert.Equal(t, s, mgr.requests[i].TargetSpec.Components[0].External.ID)
	}
}
