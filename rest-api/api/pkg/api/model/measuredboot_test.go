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

func TestAPIMeasuredBootTrustedMachineCreateRequest_Validate(t *testing.T) {
	cases := []struct {
		name         string
		machineID    string
		approvalType string
		wantError    string
	}{
		{name: "Core machine ID", machineID: "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0", approvalType: MeasuredBootApprovalTypePersist},
		{name: "wildcard", machineID: "*", approvalType: MeasuredBootApprovalTypePersist},
		{name: "malformed ID is validated by Core", machineID: "not-a-machine-id", approvalType: MeasuredBootApprovalTypePersist},
		{name: "whitespace is validated by Core", machineID: " ", approvalType: MeasuredBootApprovalTypePersist},
		{name: "missing machine ID", approvalType: MeasuredBootApprovalTypePersist, wantError: "machineId"},
		{name: "invalid approval type", machineID: "*", approvalType: "Forever", wantError: "approvalType"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			req := APIMeasuredBootTrustedMachineCreateRequest{
				SiteID:    "00000000-0000-0000-0000-000000000001",
				MachineID: tc.machineID, ApprovalType: tc.approvalType,
				PCRRegisters: "0,3,5,6", Comments: "trusted machine",
			}
			err := req.Validate()
			if tc.wantError != "" {
				require.ErrorContains(t, err, tc.wantError)
				return
			}
			require.NoError(t, err)
			protoReq := req.ToProto()
			assert.Equal(t, tc.machineID, protoReq.GetMachineId())
			assert.Equal(t, corev1.MeasurementApprovedTypePb_Persist, protoReq.GetApprovalType())
			assert.Equal(t, "0,3,5,6", protoReq.GetPcrRegisters())
			assert.Equal(t, "trusted machine", protoReq.GetComments())
		})
	}
}

func TestAPIMeasuredBootTrustedMachineDeleteRequest_Validate(t *testing.T) {
	cases := []struct {
		name      string
		selector  string
		id        string
		wantError string
	}{
		{name: "Core machine ID", selector: MeasuredBootTrustedMachineSelectorMachineID, id: "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"},
		{name: "wildcard", selector: MeasuredBootTrustedMachineSelectorMachineID, id: "*"},
		{name: "malformed ID is validated by Core", selector: MeasuredBootTrustedMachineSelectorMachineID, id: "not-a-machine-id"},
		{name: "whitespace is validated by Core", selector: MeasuredBootTrustedMachineSelectorMachineID, id: " "},
		{name: "missing machine ID", selector: MeasuredBootTrustedMachineSelectorMachineID, wantError: "ID"},
		{name: "approval UUID", selector: MeasuredBootTrustedMachineSelectorApprovalID, id: "00000000-0000-0000-0000-000000000001"},
		{name: "approval wildcard is invalid", selector: MeasuredBootTrustedMachineSelectorApprovalID, id: "*", wantError: "id"},
		{name: "machine ID is not an approval UUID", selector: MeasuredBootTrustedMachineSelectorApprovalID, id: "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0", wantError: "id"},
		{name: "invalid selector", selector: "invalid", id: "*", wantError: "invalid selector"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			req := APIMeasuredBootTrustedMachineDeleteRequest{Selector: tc.selector, ID: tc.id}
			err := req.Validate()
			if tc.wantError != "" {
				require.ErrorContains(t, err, tc.wantError)
				return
			}
			require.NoError(t, err)
			if tc.selector == MeasuredBootTrustedMachineSelectorApprovalID {
				assert.Equal(t, tc.id, req.ToProto().GetApprovalId().GetValue())
			} else {
				assert.Equal(t, tc.id, req.ToProto().GetMachineId())
			}
		})
	}
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

func TestMeasuredBootDeleteRequestSelectors(t *testing.T) {
	id := "00000000-0000-0000-0000-000000000001"

	profileRequest := APIMeasuredBootTrustedProfileDeleteRequest{Selector: MeasuredBootTrustedProfileSelectorApprovalID, ID: id}
	require.NoError(t, profileRequest.Validate())
	profileByApproval := profileRequest.ToProto()
	assert.Equal(t, id, profileByApproval.GetApprovalId().GetValue())

	profileRequest = APIMeasuredBootTrustedProfileDeleteRequest{Selector: MeasuredBootTrustedProfileSelectorProfileID, ID: id}
	require.NoError(t, profileRequest.Validate())
	profileByProfile := profileRequest.ToProto()
	assert.Equal(t, id, profileByProfile.GetProfileId().GetValue())

	profileRequest = APIMeasuredBootTrustedProfileDeleteRequest{Selector: MeasuredBootTrustedProfileSelectorProfileID, ID: "*"}
	assert.Error(t, profileRequest.Validate())

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
