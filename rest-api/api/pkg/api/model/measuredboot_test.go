// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestMeasuredBootTrustedMachineCreateRequest(t *testing.T) {
	req := APIMeasuredBootTrustedMachineCreateRequest{
		SiteID:       "00000000-0000-0000-0000-000000000001",
		MachineID:    "*",
		ApprovalType: MeasuredBootApprovalTypePersist,
		PCRRegisters: "0,3,5,6",
		Comments:     "trusted fleet",
	}
	require.NoError(t, req.Validate())

	protoReq := req.ToProto()
	assert.Equal(t, "*", protoReq.GetMachineId())
	assert.Equal(t, corev1.MeasurementApprovedTypePb_Persist, protoReq.GetApprovalType())
	assert.Equal(t, "0,3,5,6", protoReq.GetPcrRegisters())
	assert.Equal(t, "trusted fleet", protoReq.GetComments())
}

func TestMeasuredBootTrustedMachineCreateRequestRejectsInvalidMachine(t *testing.T) {
	req := APIMeasuredBootTrustedMachineCreateRequest{
		SiteID:       "00000000-0000-0000-0000-000000000001",
		MachineID:    "not-a-machine-id",
		ApprovalType: MeasuredBootApprovalTypeOneshot,
	}
	assert.ErrorContains(t, req.Validate(), "machineId")
}

func TestMeasuredBootTrustedProfileCreateRequest(t *testing.T) {
	req := APIMeasuredBootTrustedProfileCreateRequest{
		SiteID:       "00000000-0000-0000-0000-000000000001",
		ProfileID:    "00000000-0000-0000-0000-000000000002",
		ApprovalType: MeasuredBootApprovalTypeOneshot,
	}
	require.NoError(t, req.Validate())

	protoReq := req.ToProto()
	assert.Equal(t, req.ProfileID, protoReq.GetProfileId().GetValue())
	assert.Equal(t, corev1.MeasurementApprovedTypePb_Oneshot, protoReq.GetApprovalType())
	assert.Nil(t, protoReq.PcrRegisters)
	assert.Nil(t, protoReq.Comments)
}

func TestMeasuredBootCreateRequestRejectsInvalidApprovalType(t *testing.T) {
	req := APIMeasuredBootTrustedMachineCreateRequest{
		SiteID:       "00000000-0000-0000-0000-000000000001",
		MachineID:    "00000000-0000-0000-0000-000000000002",
		ApprovalType: "Forever",
	}
	assert.ErrorContains(t, req.Validate(), "approvalType")
}

func TestMeasuredBootDeleteRequestSelectors(t *testing.T) {
	id := "00000000-0000-0000-0000-000000000001"

	machineRequest := APIMeasuredBootTrustedMachineDeleteRequest{Selector: MeasuredBootTrustedMachineSelectorApprovalID, ID: id}
	require.NoError(t, machineRequest.Validate())
	machineByApproval := machineRequest.ToProto()
	assert.Equal(t, id, machineByApproval.GetApprovalId().GetValue())

	machineRequest = APIMeasuredBootTrustedMachineDeleteRequest{Selector: MeasuredBootTrustedMachineSelectorMachineID, ID: id}
	require.NoError(t, machineRequest.Validate())
	machineByMachine := machineRequest.ToProto()
	assert.Equal(t, id, machineByMachine.GetMachineId())

	profileRequest := APIMeasuredBootTrustedProfileDeleteRequest{Selector: MeasuredBootTrustedProfileSelectorApprovalID, ID: id}
	require.NoError(t, profileRequest.Validate())
	profileByApproval := profileRequest.ToProto()
	assert.Equal(t, id, profileByApproval.GetApprovalId().GetValue())

	profileRequest = APIMeasuredBootTrustedProfileDeleteRequest{Selector: MeasuredBootTrustedProfileSelectorProfileID, ID: id}
	require.NoError(t, profileRequest.Validate())
	profileByProfile := profileRequest.ToProto()
	assert.Equal(t, id, profileByProfile.GetProfileId().GetValue())

	machineRequest = APIMeasuredBootTrustedMachineDeleteRequest{Selector: MeasuredBootTrustedMachineSelectorMachineID, ID: "*"}
	require.NoError(t, machineRequest.Validate())
	assert.Equal(t, "*", machineRequest.ToProto().GetMachineId())

	machineRequest = APIMeasuredBootTrustedMachineDeleteRequest{Selector: MeasuredBootTrustedMachineSelectorApprovalID, ID: "*"}
	assert.Error(t, machineRequest.Validate())

	profileRequest = APIMeasuredBootTrustedProfileDeleteRequest{Selector: MeasuredBootTrustedProfileSelectorProfileID, ID: "*"}
	assert.Error(t, profileRequest.Validate())

	machineRequest = APIMeasuredBootTrustedMachineDeleteRequest{Selector: "invalid", ID: id}
	assert.ErrorContains(t, machineRequest.Validate(), "invalid selector")
}

func TestMeasuredBootResponsesFromProto(t *testing.T) {
	created := time.Date(2026, 7, 13, 20, 0, 0, 0, time.UTC)
	machineRecord := &corev1.MeasurementApprovedMachineRecordPb{
		ApprovalId:   &corev1.MeasurementApprovedMachineId{Value: "00000000-0000-0000-0000-000000000001"},
		MachineId:    "00000000-0000-0000-0000-000000000002",
		ApprovalType: corev1.MeasurementApprovedTypePb_Persist,
		PcrRegisters: "0,7",
		Comments:     "trusted machine",
		Ts:           timestamppb.New(created),
	}
	machine := NewAPIMeasuredBootTrustedMachine(machineRecord)
	require.NotNil(t, machine)
	assert.Equal(t, MeasuredBootApprovalTypePersist, machine.ApprovalType)
	assert.Equal(t, created, *machine.Created)

	profileRecord := &corev1.MeasurementApprovedProfileRecordPb{
		ApprovalId:   &corev1.MeasurementApprovedProfileId{Value: "00000000-0000-0000-0000-000000000003"},
		ProfileId:    &corev1.MeasurementSystemProfileId{Value: "00000000-0000-0000-0000-000000000004"},
		ApprovalType: corev1.MeasurementApprovedTypePb_Oneshot,
		Ts:           timestamppb.New(created),
	}
	profile := NewAPIMeasuredBootTrustedProfile(profileRecord)
	require.NotNil(t, profile)
	assert.Equal(t, MeasuredBootApprovalTypeOneshot, profile.ApprovalType)
	assert.Equal(t, created, *profile.Created)

	var machines APIMeasuredBootTrustedMachines
	machines.FromProto([]*corev1.MeasurementApprovedMachineRecordPb{machineRecord, nil})
	require.Len(t, machines, 2)
	assert.Equal(t, machine, machines[0])
	assert.Nil(t, machines[1])

	var profiles APIMeasuredBootTrustedProfiles
	profiles.FromProto([]*corev1.MeasurementApprovedProfileRecordPb{profileRecord, nil})
	require.Len(t, profiles, 2)
	assert.Equal(t, profile, profiles[0])
	assert.Nil(t, profiles[1])
}

func TestMeasuredBootResponsesAlwaysIncludeFields(t *testing.T) {
	tests := []struct {
		name     string
		response any
		expected string
	}{
		{
			name:     "machine",
			response: APIMeasuredBootTrustedMachine{},
			expected: `{"approvalId":"","machineId":"","approvalType":"","pcrRegisters":"","comments":"","created":null}`,
		},
		{
			name:     "profile",
			response: APIMeasuredBootTrustedProfile{},
			expected: `{"approvalId":"","profileId":"","approvalType":"","pcrRegisters":"","comments":"","created":null}`,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data, err := json.Marshal(tt.response)
			require.NoError(t, err)
			assert.JSONEq(t, tt.expected, string(data))
		})
	}
}
