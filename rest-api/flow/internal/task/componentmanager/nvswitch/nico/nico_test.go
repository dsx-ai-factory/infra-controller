// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/readiness"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestInjectExpectation(t *testing.T) {
	testCases := map[string]struct {
		client      nicoapi.Client
		info        operations.InjectExpectationTaskInfo
		expectError bool
		errContains string
	}{
		"success": {
			client: nicoapi.NewMockClient(),
			info: operations.InjectExpectationTaskInfo{
				Info: mustMarshal(t, nicoapi.AddExpectedSwitchRequest{
					BMCMACAddress:      "aa:bb:cc:dd:ee:ff",
					BMCUsername:        "admin",
					BMCPassword:        "password",
					SwitchSerialNumber: "SW-SN-001",
				}),
			},
			expectError: false,
		},
		"invalid json returns error": {
			client: nicoapi.NewMockClient(),
			info: operations.InjectExpectationTaskInfo{
				Info: json.RawMessage(`{bad-json`),
			},
			expectError: true,
			errContains: "failed to unmarshal",
		},
		"nil client returns error": {
			client: nil,
			info: operations.InjectExpectationTaskInfo{
				Info: mustMarshal(t, nicoapi.AddExpectedSwitchRequest{
					BMCMACAddress: "aa:bb:cc:dd:ee:ff",
				}),
			},
			expectError: true,
			errContains: "nico client is not configured",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			m := New(tc.client, nil)

			target := common.Target{
				Type:         devicetypes.ComponentTypeNVSwitch,
				ComponentIDs: []string{"switch-1"},
			}

			err := m.InjectExpectation(context.Background(), target, tc.info)
			if tc.expectError {
				assert.Error(t, err)
				if tc.errContains != "" {
					assert.Contains(t, err.Error(), tc.errContains)
				}
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestNormalizeDecommissionState(t *testing.T) {
	testCases := map[string]struct {
		raw  string
		want string
	}{
		"terminal state": {
			raw:  `{"state":"decommissioning","decommissioning_state":{"state":"decommissioned"}}`,
			want: "Decommissioned",
		},
		"in-progress state": {
			raw:  `{"state":"decommissioning","decommissioning_state":{"state":"factoryresetbmc"}}`,
			want: "Decommissioning/factoryresetbmc",
		},
		"unrelated state remains unchanged": {
			raw:  `{"state":"ready"}`,
			want: `{"state":"ready"}`,
		},
		"malformed state remains unchanged": {
			raw:  "Ready",
			want: "Ready",
		},
		"decommissioning state without substate remains unchanged": {
			raw:  `{"state":"decommissioning"}`,
			want: `{"state":"decommissioning"}`,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, tc.want, normalizeDecommissionState(tc.raw))
		})
	}
}

func TestPowerControl(t *testing.T) {
	m := New(nicoapi.NewMockClient(), nil)

	target := common.Target{
		Type:         devicetypes.ComponentTypeNVSwitch,
		ComponentIDs: []string{"switch-1", "switch-2"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	})
	assert.NoError(t, err)
}

func TestFirmwareControl(t *testing.T) {
	client := nicoapi.NewMockClient()
	m := New(client, nil)

	target := common.Target{
		Type:         devicetypes.ComponentTypeNVSwitch,
		ComponentIDs: []string{"switch-1"},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		TargetVersion: "2.0.0",
		AccessToken:   "switch-token",
	})
	assert.NoError(t, err)
	assert.Equal(
		t,
		"switch-token",
		client.LastUpdateComponentFirmwareRequest().GetAccessToken(),
	)
}

func TestGetFirmwareStatus(t *testing.T) {
	m := New(nicoapi.NewMockClient(), nil)

	target := common.Target{
		Type:         devicetypes.ComponentTypeNVSwitch,
		ComponentIDs: []string{"switch-1"},
	}

	statuses, err := m.GetFirmwareStatus(context.Background(), target)
	assert.NoError(t, err)
	assert.NotNil(t, statuses)
}

func TestAggregateNICoStatuses(t *testing.T) {
	mkStatus := func(compID string, state corev1.FirmwareUpdateState, errMsg string) *corev1.FirmwareUpdateStatus {
		return &corev1.FirmwareUpdateStatus{
			Result: &corev1.ComponentResult{
				ComponentId: &compID,
				Error:       errMsg,
			},
			State: state,
		}
	}

	tests := map[string]struct {
		statuses      []*corev1.FirmwareUpdateStatus
		expectedState operations.FirmwareUpdateState
		expectedError string
	}{
		"empty returns unknown": {
			statuses:      nil,
			expectedState: operations.FirmwareUpdateStateUnknown,
		},
		"all completed": {
			statuses: []*corev1.FirmwareUpdateStatus{
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
			},
			expectedState: operations.FirmwareUpdateStateCompleted,
		},
		"any failure marks overall failed": {
			statuses: []*corev1.FirmwareUpdateStatus{
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_FAILED, "BMC update failed"),
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
			},
			expectedState: operations.FirmwareUpdateStateFailed,
			expectedError: "BMC update failed",
		},
		"last completed but earlier failed still reports failed": {
			statuses: []*corev1.FirmwareUpdateStatus{
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_FAILED, "CPLD flash error"),
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
			},
			expectedState: operations.FirmwareUpdateStateFailed,
			expectedError: "CPLD flash error",
		},
		"cancelled treated as failed": {
			statuses: []*corev1.FirmwareUpdateStatus{
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_CANCELLED, ""),
			},
			expectedState: operations.FirmwareUpdateStateFailed,
		},
		"still in progress": {
			statuses: []*corev1.FirmwareUpdateStatus{
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_IN_PROGRESS, ""),
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_QUEUED, ""),
			},
			expectedState: operations.FirmwareUpdateStateQueued,
		},
		"single completed": {
			statuses: []*corev1.FirmwareUpdateStatus{
				mkStatus("sw-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
			},
			expectedState: operations.FirmwareUpdateStateCompleted,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			result := aggregateNICoStatuses("sw-1", tc.statuses)
			require.Equal(t, "sw-1", result.ComponentID)
			require.Equal(t, tc.expectedState, result.State, "expected state %v, got %v", tc.expectedState, result.State)
			if tc.expectedError != "" {
				assert.Contains(t, result.Error, tc.expectedError)
			}
		})
	}
}

// newManagerForReadinessTest builds a Manager with a tight-timeout
// readiness gate backed by the supplied MemReader so the wait loop
// actually times out within the test budget. The caller seeds the
// reader with the rack→hosts mapping and any ComponentOperationStatus rows the
// test scenario requires.
func newManagerForReadinessTest(t *testing.T, client nicoapi.Client, reader *readiness.MemReader) *Manager {
	t.Helper()
	gate := readiness.NewDBGate(reader, 50*time.Millisecond, 10*time.Millisecond)
	return New(client, gate)
}

// inUseStatus returns a status that blocks every disruptive operation,
// mirroring what inventorysync would persist for a tenant-attached host.
func inUseStatus() *types.ComponentOperationStatus {
	return &types.ComponentOperationStatus{
		Phase:  types.PhaseInUse,
		Reason: "tenant attached",
		BlockedOperations: []types.OperationType{
			types.OperationTypePowerControl,
			types.OperationTypeFirmwareControl,
		},
	}
}

func TestPowerControl_RefusesWhenRackHostInUse(t *testing.T) {
	client := nicoapi.NewMockClient()
	client.SetSwitchRackID("sw-1", "rack-A")

	reader := readiness.NewMemReader()
	reader.SetRackHosts("rack-A", []string{"host-1"})
	reader.SetStatus("host-1", inUseStatus())

	m := newManagerForReadinessTest(t, client, reader)
	target := common.Target{
		Type:         devicetypes.ComponentTypeNVSwitch,
		ComponentIDs: []string{"sw-1"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOff,
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "refused")
	assert.Contains(t, err.Error(), "timed out")
	assert.Contains(t, err.Error(), "host-1")
}

func TestPowerControl_AllowsWhenRackHostsReady(t *testing.T) {
	client := nicoapi.NewMockClient()
	client.SetSwitchRackID("sw-1", "rack-A")

	reader := readiness.NewMemReader()
	reader.SetRackHosts("rack-A", []string{"host-1"})
	reader.SetStatus("host-1", &types.ComponentOperationStatus{Phase: types.PhaseReady})

	m := newManagerForReadinessTest(t, client, reader)
	target := common.Target{
		Type:         devicetypes.ComponentTypeNVSwitch,
		ComponentIDs: []string{"sw-1"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	})
	require.NoError(t, err)
}

func TestFirmwareControl_RefusesWhenRackHostInUse(t *testing.T) {
	client := nicoapi.NewMockClient()
	client.SetSwitchRackID("sw-1", "rack-A")

	reader := readiness.NewMemReader()
	reader.SetRackHosts("rack-A", []string{"host-1"})
	reader.SetStatus("host-1", inUseStatus())

	m := newManagerForReadinessTest(t, client, reader)
	target := common.Target{
		Type:         devicetypes.ComponentTypeNVSwitch,
		ComponentIDs: []string{"sw-1"},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		Operation:     operations.FirmwareOperationUpgrade,
		TargetVersion: "1.0.0",
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "refused")
	assert.Contains(t, err.Error(), "timed out")
}

// TestPowerControl_OverrideBypassesReadinessCheck verifies that
// OverrideReadinessCheck short-circuits the rack-scoped gate on
// NVSwitch PowerControl. A host on the resolved rack is reported as
// in-use — which would otherwise block the call — yet the operation
// is expected to proceed past the gate.
func TestPowerControl_OverrideBypassesReadinessCheck(t *testing.T) {
	client := nicoapi.NewMockClient()
	client.SetSwitchRackID("sw-1", "rack-A")

	reader := readiness.NewMemReader()
	reader.SetRackHosts("rack-A", []string{"host-1"})
	reader.SetStatus("host-1", inUseStatus())

	m := newManagerForReadinessTest(t, client, reader)
	target := common.Target{
		Type:         devicetypes.ComponentTypeNVSwitch,
		ComponentIDs: []string{"sw-1"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation:              operations.PowerOperationPowerOn,
		OverrideReadinessCheck: true,
	})
	require.NoError(t, err)
}

func mustMarshal(t *testing.T, v any) json.RawMessage {
	t.Helper()
	data, err := json.Marshal(v)
	if err != nil {
		t.Fatalf("failed to marshal: %v", err)
	}
	return data
}
