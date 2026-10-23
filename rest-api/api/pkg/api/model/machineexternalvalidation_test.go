// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

func TestAPIGetAllMachineExternalValidationHoldRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIGetAllMachineExternalValidationHoldRequest
		wantErr bool
	}{
		{name: "Site ID accepted", request: APIGetAllMachineExternalValidationHoldRequest{SiteID: uuid.NewString()}},
		{name: "missing Site ID rejected", wantErr: true},
		{name: "malformed Site ID rejected", request: APIGetAllMachineExternalValidationHoldRequest{SiteID: "site-1"}, wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			assert.Equal(t, tt.wantErr, err != nil)
		})
	}
}

func TestAPIMachineExternalValidationAttemptCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIMachineExternalValidationAttemptCreateRequest
		wantErr bool
	}{
		{name: "idempotency key accepted", request: APIMachineExternalValidationAttemptCreateRequest{IdempotencyKey: uuid.NewString()}},
		{name: "missing idempotency key rejected", wantErr: true},
		{name: "malformed idempotency key rejected", request: APIMachineExternalValidationAttemptCreateRequest{IdempotencyKey: "start-1"}, wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			assert.Equal(t, tt.wantErr, err != nil)
		})
	}
}

func TestAPIMachineExternalValidationAttemptCompleteRequest_Validate(t *testing.T) {
	instanceID := uuid.NewString()

	tests := []struct {
		name    string
		request APIMachineExternalValidationAttemptCompleteRequest
		wantErr bool
	}{
		{
			name:    "result without details accepted",
			request: APIMachineExternalValidationAttemptCompleteRequest{Outcome: MachineExternalValidationOutcomeCancelled, InstanceID: instanceID},
		},
		{
			name: "details at the byte limit accepted",
			request: APIMachineExternalValidationAttemptCompleteRequest{
				Outcome:    MachineExternalValidationOutcomePassed,
				InstanceID: instanceID,
				Details:    cutil.GetPtr(strings.Repeat("a", maxMachineExternalValidationTextBytes)),
			},
		},
		{
			name: "multi-byte details over the byte limit rejected",
			request: APIMachineExternalValidationAttemptCompleteRequest{
				Outcome:    MachineExternalValidationOutcomeFailed,
				InstanceID: instanceID,
				Details:    cutil.GetPtr(strings.Repeat("é", maxMachineExternalValidationTextBytes/2+1)),
			},
			wantErr: true,
		},
		{
			name:    "missing outcome rejected",
			request: APIMachineExternalValidationAttemptCompleteRequest{InstanceID: instanceID},
			wantErr: true,
		},
		{
			name:    "unknown outcome rejected",
			request: APIMachineExternalValidationAttemptCompleteRequest{Outcome: "Skipped", InstanceID: instanceID},
			wantErr: true,
		},
		{
			name:    "missing Instance ID rejected",
			request: APIMachineExternalValidationAttemptCompleteRequest{Outcome: MachineExternalValidationOutcomePassed},
			wantErr: true,
		},
		{
			name:    "malformed Instance ID rejected",
			request: APIMachineExternalValidationAttemptCompleteRequest{Outcome: MachineExternalValidationOutcomePassed, InstanceID: "instance-1"},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			assert.Equal(t, tt.wantErr, err != nil)
		})
	}
}

func TestAPIMachineExternalValidationAttemptCancelRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIMachineExternalValidationAttemptCancelRequest
		wantErr bool
	}{
		{name: "details accepted", request: APIMachineExternalValidationAttemptCancelRequest{Details: "Validation run aborted"}},
		{name: "missing details rejected", wantErr: true},
		{
			name:    "details over the byte limit rejected",
			request: APIMachineExternalValidationAttemptCancelRequest{Details: strings.Repeat("a", maxMachineExternalValidationTextBytes+1)},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			assert.Equal(t, tt.wantErr, err != nil)
		})
	}
}

func TestAPIMachineExternalValidationHoldDeleteRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIMachineExternalValidationHoldDeleteRequest
		wantErr bool
	}{
		{name: "reason accepted", request: APIMachineExternalValidationHoldDeleteRequest{Reason: "Machine is exempt from external validation"}},
		{name: "missing reason rejected", wantErr: true},
		{
			name:    "reason over the byte limit rejected",
			request: APIMachineExternalValidationHoldDeleteRequest{Reason: strings.Repeat("a", maxMachineExternalValidationTextBytes+1)},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			assert.Equal(t, tt.wantErr, err != nil)
		})
	}
}
