// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package leakdetection

import (
	"context"
	"encoding/json"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
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

func TestSubmitPowerOffTask(t *testing.T) {
	cases := map[string]struct {
		componentType devicetypes.ComponentType
		manager       mockManager
		wantError     string
	}{
		"compute":            {componentType: devicetypes.ComponentTypeCompute},
		"switch":             {componentType: devicetypes.ComponentTypeNVSwitch},
		"no tasks":           {manager: mockManager{returnNoTaskID: true}, wantError: "failed to create any power-off tasks"},
		"submission failure": {manager: mockManager{submitErr: errors.New("submit failed")}, wantError: "submit failed"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			err := submitPowerOffTask(context.Background(), &tc.manager, "component-1", tc.componentType)
			if tc.wantError != "" {
				require.ErrorContains(t, err, tc.wantError)
				return
			}
			require.NoError(t, err)
			require.Len(t, tc.manager.requests, 1)
			req := tc.manager.requests[0]
			require.True(t, req.TargetSpec.IsComponentTargeting())
			require.Len(t, req.TargetSpec.Components, 1)
			comp := req.TargetSpec.Components[0]
			assert.Equal(t, uuid.Nil, comp.UUID)
			require.NotNil(t, comp.External)
			assert.Equal(t, tc.componentType, comp.External.Type)
			assert.Equal(t, "component-1", comp.External.ID)
			assert.Equal(t, operation.ConflictStrategyPreempt, req.ConflictStrategy)
			assert.Contains(t, req.Description, "component-1")
			var info operations.PowerControlTaskInfo
			require.NoError(t, json.Unmarshal(req.Operation.Info, &info))
			assert.Equal(t, operations.PowerOperationForcePowerOff, info.Operation)
			assert.True(t, info.Forced)
			assert.True(t, info.OverrideReadinessCheck)
		})
	}
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
