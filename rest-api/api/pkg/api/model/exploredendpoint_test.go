// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/durationpb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestAPIExploredEndpointGetAllRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APIExploredEndpointGetAllRequest
		wantErr string
	}{
		{
			name: "valid siteId",
			req:  APIExploredEndpointGetAllRequest{SiteID: "00000000-0000-0000-0000-000000000001"},
		},
		{
			name:    "missing siteId",
			req:     APIExploredEndpointGetAllRequest{},
			wantErr: "a value is required",
		},
		{
			name:    "invalid siteId",
			req:     APIExploredEndpointGetAllRequest{SiteID: "not-a-uuid"},
			wantErr: "must be a valid UUID",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				assert.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestNewAPIExploredEndpoint_FromProto(t *testing.T) {
	machineID := "00000000-0000-0000-0000-0000000000aa"
	lastErr := `{"error":"timeout"}`
	vendor := "NVIDIA"
	mitigation := "retry exploration"
	mac := "00:11:22:33:44:55"
	enabled := true
	ifaceID := "1"

	protoEP := &corev1.ExploredEndpoint{
		Address:               "10.0.0.1",
		ReportVersion:         "3",
		ExplorationRequested:  true,
		PreingestionState:     "WaitingForNetwork",
		LastRedfishBmcReset:   "2026-01-01T00:00:00Z",
		LastIpmitoolBmcReset:  "2026-01-02T00:00:00Z",
		LastRedfishReboot:     "2026-01-03T00:00:00Z",
		LastRedfishPowercycle: "2026-01-04T00:00:00Z",
		PauseRemediation:      true,
		Warnings: []string{
			"DPU OOB interface is missing from the exploration report.",
		},
		Report: &corev1.EndpointExplorationReport{
			EndpointType:           "HostBmc",
			LastExplorationError:   &lastErr,
			MachineId:              &machineID,
			LastExplorationLatency: durationpb.New(1500 * time.Millisecond),
			Vendor:                 &vendor,
			FirmwareVersions: map[string]string{
				"bmc": "25.06-2",
			},
			LastExplorationErrorSchema: &corev1.OperatorErrorSchema{
				ErrorCode:  "exploration_timeout",
				Mitigation: &mitigation,
				Text:       "endpoint timed out",
			},
			Managers: []*corev1.Manager{
				{
					Id: "BMC",
					EthernetInterfaces: []*corev1.EthernetInterface{
						{
							Id:               &ifaceID,
							MacAddress:       &mac,
							InterfaceEnabled: &enabled,
						},
					},
				},
			},
			Systems: []*corev1.ComputerSystem{
				{
					Id:         "System.1",
					PowerState: corev1.ComputerSystemPowerState_On,
					Attributes: &corev1.ComputerSystemAttributes{
						NicMode: corev1.NicMode_DPU.Enum(),
					},
				},
			},
			SecureBootStatus: &corev1.SecureBootStatus{IsEnabled: true},
			LockdownStatus: &corev1.LockdownStatus{
				Status:  corev1.InternalLockdownStatus_PARTIAL,
				Message: "partial lockdown",
			},
			MachineSetupStatus: &corev1.MachineSetupStatus{
				IsDone: true,
				Diffs: []*corev1.MachineSetupDiff{
					{Key: "boot", Expected: "a", Actual: "b"},
				},
				EvaluatedBootInterface: &corev1.MachineBootInterfaceTarget{
					Target: &corev1.MachineBootInterfaceTarget_Pair{
						Pair: &corev1.MachineBootInterfacePair{
							MacAddress:  mac,
							InterfaceId: ifaceID,
						},
					},
				},
			},
		},
	}

	got := NewAPIExploredEndpoint(protoEP)
	require.NotNil(t, got)
	assert.Equal(t, "10.0.0.1", got.Address)
	assert.Equal(t, "3", got.ReportVersion)
	assert.True(t, got.ExplorationRequested)
	assert.Equal(t, "WaitingForNetwork", got.PreingestionState)
	assert.True(t, got.PauseRemediation)
	assert.Equal(t, protoEP.Warnings, got.Warnings)
	require.NotNil(t, got.Report)
	assert.Equal(t, "HostBmc", got.Report.EndpointType)
	require.NotNil(t, got.Report.LastExplorationError)
	assert.Equal(t, lastErr, *got.Report.LastExplorationError)
	require.NotNil(t, got.Report.MachineID)
	assert.Equal(t, machineID, *got.Report.MachineID)
	require.NotNil(t, got.Report.LastExplorationLatency)
	assert.Equal(t, "1.5s", *got.Report.LastExplorationLatency)
	require.NotNil(t, got.Report.Vendor)
	assert.Equal(t, "NVIDIA", *got.Report.Vendor)
	assert.Equal(t, map[string]string{"bmc": "25.06-2"}, got.Report.FirmwareVersions)
	require.NotNil(t, got.Report.LastExplorationErrorSchema)
	assert.Equal(t, "exploration_timeout", got.Report.LastExplorationErrorSchema.ErrorCode)
	require.Len(t, got.Report.Managers, 1)
	assert.Equal(t, "BMC", got.Report.Managers[0].ID)
	require.Len(t, got.Report.Managers[0].EthernetInterfaces, 1)
	assert.Equal(t, mac, *got.Report.Managers[0].EthernetInterfaces[0].MacAddress)
	require.Len(t, got.Report.Systems, 1)
	assert.Equal(t, APIExploredComputerSystemPowerState("On"), got.Report.Systems[0].PowerState)
	require.NotNil(t, got.Report.Systems[0].Attributes)
	require.NotNil(t, got.Report.Systems[0].Attributes.NicMode)
	assert.Equal(t, APIExploredNicMode("Dpu"), *got.Report.Systems[0].Attributes.NicMode)
	require.NotNil(t, got.Report.SecureBootStatus)
	assert.True(t, got.Report.SecureBootStatus.IsEnabled)
	require.NotNil(t, got.Report.LockdownStatus)
	assert.Equal(t, APIExploredInternalLockdownStatus("Partial"), got.Report.LockdownStatus.Status)
	require.NotNil(t, got.Report.MachineSetupStatus)
	assert.True(t, got.Report.MachineSetupStatus.IsDone)
	require.NotNil(t, got.Report.MachineSetupStatus.EvaluatedBootInterface)
	require.NotNil(t, got.Report.MachineSetupStatus.EvaluatedBootInterface.Pair)
	assert.Equal(t, mac, got.Report.MachineSetupStatus.EvaluatedBootInterface.Pair.MacAddress)
}

func TestNewAPIExploredEndpoint_Nil(t *testing.T) {
	assert.Nil(t, NewAPIExploredEndpoint(nil))
}

func TestAPIMachineBootInterfaceTarget_FromProto(t *testing.T) {
	target := &corev1.MachineBootInterfaceTarget{
		Target: &corev1.MachineBootInterfaceTarget_MacOnly{MacOnly: ""},
	}

	got := &APIMachineBootInterfaceTarget{}
	got.FromProto(target)
	require.NotNil(t, got.MacOnly)
	assert.Empty(t, *got.MacOnly)
	assert.Nil(t, got.Pair)
}

func TestExploredEnum_FromProto(t *testing.T) {
	var nicMode APIExploredNicMode
	nicMode.FromProto(corev1.NicMode(99))
	assert.Equal(t, APIExploredNicMode("Unknown"), nicMode)

	var powerState APIExploredComputerSystemPowerState
	powerState.FromProto(corev1.ComputerSystemPowerState(99))
	assert.Equal(t, APIExploredComputerSystemPowerState("Unknown"), powerState)

	var lockdownStatus APIExploredInternalLockdownStatus
	lockdownStatus.FromProto(corev1.InternalLockdownStatus(99))
	assert.Equal(t, APIExploredInternalLockdownStatus("Unknown"), lockdownStatus)
}

func TestAPIExploredEndpoint_ResponseFieldsAreNotOmitted(t *testing.T) {
	for _, tc := range []struct {
		name     string
		warnings []string
	}{
		{name: "nil warnings"},
		{name: "empty warnings", warnings: []string{}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			endpoint := NewAPIExploredEndpoint(&corev1.ExploredEndpoint{Warnings: tc.warnings})
			require.NotNil(t, endpoint.Warnings)
			data, err := json.Marshal(endpoint)
			require.NoError(t, err)
			assert.JSONEq(t, `{
		"address":"", "report":null, "reportVersion":"", "explorationRequested":false,
		"preingestionState":"", "lastRedfishBmcReset":"", "lastIpmitoolBmcReset":"",
		"lastRedfishReboot":"", "lastRedfishPowercycle":"", "pauseRemediation":false,
		"warnings":[]
	}`, string(data))
		})
	}
}
