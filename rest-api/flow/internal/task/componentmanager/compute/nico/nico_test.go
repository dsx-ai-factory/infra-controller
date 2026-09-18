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
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/compute/common/dpureprov"
	nicoprovider "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providers/nico"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/readiness"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestDescriptor(t *testing.T) {
	d := Descriptor()

	assert.Equal(t, devicetypes.ComponentTypeCompute, d.Type)
	assert.Equal(t, ImplementationName, d.Implementation)
	assert.ElementsMatch(t, []string{nicoprovider.ProviderName}, d.RequiredProviders)
	assert.ElementsMatch(t,
		capability.CapabilitySet{
			capability.CapabilityBringUpControl,
			capability.CapabilityBringUpStatus,
			capability.CapabilityDecommissionControl,
			capability.CapabilityDecommissionStatus,
			capability.CapabilityFirmwareControl,
			capability.CapabilityFirmwareStatus,
			capability.CapabilityInjectExpectation,
			capability.CapabilityPowerControl,
			capability.CapabilityPowerStatus,
		},
		d.Capabilities,
	)
}

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
				Info: mustMarshal(t, nicoapi.AddExpectedMachineRequest{
					BMCMACAddress:       "aa:bb:cc:dd:ee:ff",
					BMCUsername:         "admin",
					BMCPassword:         "password",
					ChassisSerialNumber: "SN12345",
				}),
			},
			expectError: false,
		},
		"invalid json returns error": {
			client: nicoapi.NewMockClient(),
			info: operations.InjectExpectationTaskInfo{
				Info: json.RawMessage(`{invalid`),
			},
			expectError: true,
			errContains: "failed to unmarshal",
		},
		"nil client returns error": {
			client: nil,
			info: operations.InjectExpectationTaskInfo{
				Info: mustMarshal(t, nicoapi.AddExpectedMachineRequest{
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
				Type:         devicetypes.ComponentTypeCompute,
				ComponentIDs: []string{"machine-1"},
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

func TestPowerControl_HappyPath(t *testing.T) {
	m := New(nicoapi.NewMockClient(), nil)

	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1", "machine-2"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	})
	require.NoError(t, err)
}

func TestPowerControl_RejectsUnsupportedOperation(t *testing.T) {
	m := New(nicoapi.NewMockClient(), nil)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperation(0xff),
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "unsupported power operation")
}

func TestFirmwareControl_HappyPath(t *testing.T) {
	client := nicoapi.NewMockClient()
	m := New(client, nil)

	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1"},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		Operation:     operations.FirmwareOperationUpgrade,
		TargetVersion: "fw-bundle-id-v1",
		SubTargets:    []string{"bmc", "bios"},
		AccessToken:   "compute-token",
	})
	require.NoError(t, err)
	require.Equal(
		t,
		"compute-token",
		client.LastUpdateComponentFirmwareRequest().GetAccessToken(),
	)
}

func TestFirmwareControl_RejectsUnknownSubTarget(t *testing.T) {
	m := New(nicoapi.NewMockClient(), nil)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1"},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		Operation:  operations.FirmwareOperationUpgrade,
		SubTargets: []string{"made-up"},
	})
	require.Error(t, err)
}

// TestFirmwareControl_DpuOnlyTarget pins the DPU-only branch contract:
// when SubTargets is exactly ["dpu"], compute/nico must NOT call
// UpdateComponentFirmware at all (an empty Components list there would
// be interpreted by Core as "update everything in the bundle"), but
// must still drive the DPU reprovisioning SAGA.
func TestFirmwareControl_DpuOnlyTarget(t *testing.T) {
	client := nicoapi.NewMockClient()
	client.SetHostDpuMachineIds(testHostMachineID, []string{"dpu-1a"})
	client.SetHostInstanceID(testHostMachineID, "inst-1")

	m := withFastDpuReprov(New(client, nil), client)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{testHostMachineID},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		Operation:     operations.FirmwareOperationUpgrade,
		TargetVersion: "fw-bundle-id-v1",
		SubTargets:    []string{"dpu"},
	})
	require.NoError(t, err)

	triggers := client.DpuReprovisioningTriggers()
	require.Len(t, triggers, 1)
	assert.Equal(t, testHostMachineID, triggers[0].MachineID)
	assert.True(t, triggers[0].UpdateFirmware)

	// Override is removed via deferred cleanup.
	assert.Empty(t, client.HostUpdateOverridesActive())

	// Assigned host -> InvokeInstancePower with apply_updates=true.
	power := client.InstancePowerCalls()
	require.Len(t, power, 1)
	assert.True(t, power[0].ApplyUpdates)
}

func TestFirmwareControl_DpuOnlyRejectsAuthenticationData(t *testing.T) {
	client := nicoapi.NewMockClient()
	m := New(client, nil)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{testHostMachineID},
	}

	err := m.FirmwareControl(
		context.Background(),
		target,
		operations.FirmwareControlTaskInfo{
			Operation:   operations.FirmwareOperationUpgrade,
			SubTargets:  []string{"dpu"},
			AccessToken: "compute-token",
		},
	)

	require.ErrorContains(
		t,
		err,
		"dpu-only firmware updates do not support authentication data",
	)
	require.Nil(t, client.LastUpdateComponentFirmwareRequest())
	require.Empty(t, client.DpuReprovisioningTriggers())
}

// TestFirmwareControl_MixedDpuAndComputeTargets pins the
// mixed-request contract: a request like ["bmc", "dpu"] runs the
// compute-tray-internal path AND the DPU SAGA, with DPU last.
func TestFirmwareControl_MixedDpuAndComputeTargets(t *testing.T) {
	client := nicoapi.NewMockClient()
	client.SetHostDpuMachineIds(testHostMachineID, []string{"dpu-1a"})
	client.SetHostInstanceID(testHostMachineID, "inst-1")

	m := withFastDpuReprov(New(client, nil), client)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{testHostMachineID},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		Operation:     operations.FirmwareOperationUpgrade,
		TargetVersion: "fw-bundle-id-v1",
		SubTargets:    []string{"bmc", "dpu"},
	})
	require.NoError(t, err)

	require.Len(t, client.DpuReprovisioningTriggers(), 1)
	assert.Empty(t, client.HostUpdateOverridesActive())
}

// TestFirmwareControl_EmptySubTargetsSkipsDpu pins the explicit-opt-in
// rule: an empty SubTargets list means "everything in the bundle" for
// compute-tray-internal targets but does NOT include DPU. The mock's
// DpuReprovisioningTriggers must remain empty.
func TestFirmwareControl_EmptySubTargetsSkipsDpu(t *testing.T) {
	client := nicoapi.NewMockClient()
	client.SetHostDpuMachineIds(testHostMachineID, []string{"dpu-1a"})

	m := New(client, nil)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{testHostMachineID},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		Operation:     operations.FirmwareOperationUpgrade,
		TargetVersion: "fw-bundle-id-v1",
		SubTargets:    nil, // empty means "no DPU"
	})
	require.NoError(t, err)

	assert.Empty(t, client.DpuReprovisioningTriggers(),
		"empty SubTargets must not trigger DPU reprovisioning")
}

func TestGetFirmwareStatus_HappyPath(t *testing.T) {
	m := New(nicoapi.NewMockClient(), nil)

	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1"},
	}

	statuses, err := m.GetFirmwareStatus(context.Background(), target)
	require.NoError(t, err)
	require.NotNil(t, statuses)
	// Mock returns no statuses, so the requested machine still appears with
	// Unknown state.
	require.Contains(t, statuses, "machine-1")
	assert.Equal(t, operations.FirmwareUpdateStateUnknown, statuses["machine-1"].State)
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
				mkStatus("machine-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
				mkStatus("machine-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
			},
			expectedState: operations.FirmwareUpdateStateCompleted,
		},
		"any failure marks overall failed": {
			statuses: []*corev1.FirmwareUpdateStatus{
				mkStatus("machine-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
				mkStatus("machine-1", corev1.FirmwareUpdateState_FW_STATE_FAILED, "BIOS update failed"),
			},
			expectedState: operations.FirmwareUpdateStateFailed,
			expectedError: "BIOS update failed",
		},
		"still in progress": {
			statuses: []*corev1.FirmwareUpdateStatus{
				mkStatus("machine-1", corev1.FirmwareUpdateState_FW_STATE_COMPLETED, ""),
				mkStatus("machine-1", corev1.FirmwareUpdateState_FW_STATE_IN_PROGRESS, ""),
			},
			expectedState: operations.FirmwareUpdateStateQueued,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			result := aggregateNICoStatuses("machine-1", tc.statuses)
			require.Equal(t, "machine-1", result.ComponentID)
			require.Equal(t, tc.expectedState, result.State)
			if tc.expectedError != "" {
				assert.Contains(t, result.Error, tc.expectedError)
			}
		})
	}
}

// newManagerForReadinessTest builds a Manager with a tight-timeout
// readiness gate backed by the supplied MemReader so the wait loop
// actually times out within the test budget. The caller seeds the
// reader with the ComponentOperationStatus rows the test expects.
func newManagerForReadinessTest(t *testing.T, client nicoapi.Client, reader *readiness.MemReader) *Manager {
	t.Helper()
	gate := readiness.NewDBGate(reader, 50*time.Millisecond, 10*time.Millisecond)
	return New(client, gate)
}

// withFastDpuReprov sets a microsecond-resolution poll interval / short
// timeout on the Manager's dpureprov.Options so DPU-branch tests don't
// sleep on the production default 30s poll cadence. Production
// callers always go through New() which leaves dpuReprovOpts zero.
func withFastDpuReprov(m *Manager, mock nicoapi.Client) *Manager {
	pollIdx := 0
	m.dpuReprovOpts = dpureprov.Options{
		PollInterval: 1 * time.Microsecond,
		PollTimeout:  10 * time.Second,
		Sleep: func(_ context.Context, _ time.Duration) error {
			// First poll always sees pending=true because
			// TriggerDpuReprovisioning seeded the mock; the second
			// "tick" flips it to false so the SAGA exits without
			// real time elapsing.
			pollIdx++
			if pollIdx == 1 {
				mock.SetDpuReprovisioningPending(testHostMachineID, false)
			}
			return nil
		},
	}
	return m
}

// testHostMachineID is the host id used by the DPU-branch tests; kept
// at package scope so the Sleep injection point in withFastDpuReprov
// can refer to it without an extra closure.
const testHostMachineID = "machine-1"

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

func TestPowerControl_RefusesInUseMachine(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", inUseStatus())

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOff,
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "refused")
	assert.Contains(t, err.Error(), "timed out")
	assert.Contains(t, err.Error(), "machine-1")
}

func TestPowerControl_AllowsReadyMachine(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", &types.ComponentOperationStatus{Phase: types.PhaseReady})

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	})
	require.NoError(t, err)
}

func TestFirmwareControl_RefusesInUseMachine(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", inUseStatus())

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1"},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		Operation:     operations.FirmwareOperationUpgrade,
		TargetVersion: "fw-v1",
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "refused")
	assert.Contains(t, err.Error(), "timed out")
}

// TestPowerControl_OverrideBypassesReadinessCheck verifies that
// OverrideReadinessCheck short-circuits the readiness gate on
// PowerControl. The host is reported as in-use — which would otherwise
// block the call — yet the operation is expected to proceed past the
// gate.
func TestPowerControl_OverrideBypassesReadinessCheck(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", inUseStatus())

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation:              operations.PowerOperationPowerOn,
		OverrideReadinessCheck: true,
	})
	require.NoError(t, err)
}

func TestBringUpControl_RefusesInUseMachine(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", inUseStatus())

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1"},
	}

	err := m.BringUpControl(context.Background(), target, operations.BringUpTaskInfo{})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "refused")
	assert.Contains(t, err.Error(), "timed out")
}

func TestBringUpControl_OverrideBypassesReadinessCheck(t *testing.T) {
	reader := readiness.NewMemReader()
	reader.SetStatus("machine-1", inUseStatus())

	m := newManagerForReadinessTest(t, nicoapi.NewMockClient(), reader)
	target := common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1"},
	}

	err := m.BringUpControl(context.Background(), target, operations.BringUpTaskInfo{
		OverrideReadinessCheck: true,
	})
	require.NoError(t, err)
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
			raw:  `{"state":"decommissioning","decommissioning_state":{"state":"factoryresettingbmcs"}}`,
			want: "Decommissioning/factoryresettingbmcs",
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

func TestGetDecommissionStatusNormalizesStates(t *testing.T) {
	client := nicoapi.NewMockClient()
	client.SetMachineControllerState(
		"machine-1",
		`{"state":"decommissioning","decommissioning_state":{"state":"decommissioned"}}`,
	)
	client.SetMachineControllerState(
		"machine-2",
		`{"state":"decommissioning","decommissioning_state":{"state":"suppressingoobdhcp"}}`,
	)

	m := New(client, nil)
	states, err := m.GetDecommissionStatus(context.Background(), common.Target{
		Type:         devicetypes.ComponentTypeCompute,
		ComponentIDs: []string{"machine-1", "machine-2", "machine-3"},
	})
	require.NoError(t, err)
	assert.Equal(t, map[string]string{
		"machine-1": "Decommissioned",
		"machine-2": "Decommissioning/suppressingoobdhcp",
		"machine-3": "",
	}, states)
}

func mustMarshal(t *testing.T, v any) json.RawMessage {
	t.Helper()
	data, err := json.Marshal(v)
	if err != nil {
		t.Fatalf("failed to marshal: %v", err)
	}
	return data
}
