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
)

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
			raw:  `{"state":"decommissioning","decommissioning_state":{"state":"suppressingsiteexplorer"}}`,
			want: "Decommissioning/suppressingsiteexplorer",
		},
		"factory reset state": {
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
				Info: mustMarshal(t, nicoapi.AddExpectedPowerShelfRequest{
					BMCMACAddress:     "11:22:33:44:55:66",
					BMCUsername:       "admin",
					BMCPassword:       "password",
					ShelfSerialNumber: "PS-SN-001",
					IPAddress:         "10.0.0.50",
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
				Info: mustMarshal(t, nicoapi.AddExpectedPowerShelfRequest{
					BMCMACAddress: "11:22:33:44:55:66",
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
				Type:        devicetypes.ComponentTypePowerShelf,
				Identifiers: []string{"ps-1"},
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

func TestPowerControl(t *testing.T) {
	m := New(nicoapi.NewMockClient(), nil)

	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"ps-1", "ps-2"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	})
	assert.NoError(t, err)
}

func TestMACTargetRequests(t *testing.T) {
	client := nicoapi.NewMockClient()
	m := New(client, nil)
	macs := []string{"aa:bb:cc:dd:ee:01", "aa:bb:cc:dd:ee:02"}
	target := common.Target{
		Type:           devicetypes.ComponentTypePowerShelf,
		IdentifierType: common.IdentifierTypeMACAddress,
		Identifiers:    macs,
	}

	require.NoError(t, m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	}))
	assert.Equal(t, macs, client.LastComponentPowerControlRequest().GetPowerShelfPmcMacs().GetMacAddresses())

	_, err := m.GetPowerStatus(context.Background(), target)
	require.NoError(t, err)
	assert.Equal(t, macs, client.LastGetComponentInventoryRequest().GetPowerShelfPmcMacs().GetMacAddresses())

	require.NoError(t, m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		TargetVersion: "1.2.3",
	}))
	assert.Equal(t, macs, client.LastUpdateComponentFirmwareRequest().GetPowerShelves().GetPmcMacs().GetMacAddresses())

	_, err = m.GetFirmwareStatus(context.Background(), target)
	require.NoError(t, err)
	assert.Equal(t, macs, client.LastGetComponentFirmwareStatusRequest().GetPowerShelfPmcMacs().GetMacAddresses())
}

func TestFirmwareControl(t *testing.T) {
	client := nicoapi.NewMockClient()
	m := New(client, nil)

	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"ps-1"},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		TargetVersion:        "1.2.3",
		AccessToken:          "powershelf-token",
		OverrideVersionCheck: true,
	})
	assert.NoError(t, err)
	assert.True(t, client.LastUpdateComponentFirmwareRequest().GetForceUpdate())
	assert.Equal(
		t,
		"powershelf-token",
		client.LastUpdateComponentFirmwareRequest().GetAccessToken(),
	)
}

func TestGetFirmwareStatus(t *testing.T) {
	m := New(nicoapi.NewMockClient(), nil)

	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"ps-1"},
	}

	statuses, err := m.GetFirmwareStatus(context.Background(), target)
	assert.NoError(t, err)
	assert.NotNil(t, statuses)
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
	client.SetPowerShelfRackID("ps-1", "rack-A")

	reader := readiness.NewMemReader()
	reader.SetRackHosts("rack-A", []string{"host-1"})
	reader.SetStatus("host-1", inUseStatus())

	m := newManagerForReadinessTest(t, client, reader)
	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"ps-1"},
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
	client.SetPowerShelfRackID("ps-1", "rack-A")

	reader := readiness.NewMemReader()
	reader.SetRackHosts("rack-A", []string{"host-1"})
	reader.SetStatus("host-1", &types.ComponentOperationStatus{Phase: types.PhaseReady})

	m := newManagerForReadinessTest(t, client, reader)
	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"ps-1"},
	}

	err := m.PowerControl(context.Background(), target, operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	})
	require.NoError(t, err)
}

func TestFirmwareControl_RefusesWhenRackHostInUse(t *testing.T) {
	client := nicoapi.NewMockClient()
	client.SetPowerShelfRackID("ps-1", "rack-A")

	reader := readiness.NewMemReader()
	reader.SetRackHosts("rack-A", []string{"host-1"})
	reader.SetStatus("host-1", inUseStatus())

	m := newManagerForReadinessTest(t, client, reader)
	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"ps-1"},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		Operation:     operations.FirmwareOperationUpgrade,
		TargetVersion: "1.0.0",
	})
	require.Error(t, err)
	assert.Contains(t, err.Error(), "refused")
	assert.Contains(t, err.Error(), "timed out")
}

// TestFirmwareControl_OverrideBypassesReadinessCheck verifies that
// OverrideReadinessCheck short-circuits the rack-scoped gate on
// PowerShelf FirmwareControl. A host on the resolved rack is reported
// as in-use — which would otherwise block the call — yet the
// operation is expected to proceed past the gate.
func TestFirmwareControl_OverrideBypassesReadinessCheck(t *testing.T) {
	client := nicoapi.NewMockClient()
	client.SetPowerShelfRackID("ps-1", "rack-A")

	reader := readiness.NewMemReader()
	reader.SetRackHosts("rack-A", []string{"host-1"})
	reader.SetStatus("host-1", inUseStatus())

	m := newManagerForReadinessTest(t, client, reader)
	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"ps-1"},
	}

	err := m.FirmwareControl(context.Background(), target, operations.FirmwareControlTaskInfo{
		Operation:              operations.FirmwareOperationUpgrade,
		TargetVersion:          "1.0.0",
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
