// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"math"
	"testing"

	"github.com/stretchr/testify/assert"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/protobuf/types/known/durationpb"
)

func TestAPIMachineValidationRunCreateRequestToProto(t *testing.T) {
	request := APIMachineValidationRunCreateRequest{
		Tags:               []string{"history"},
		AllowedTests:       []string{"gpu_bandwidth"},
		RunUnverifiedTests: true,
		Contexts:           []string{"OnDemand"},
	}

	protoRequest := request.ToProto("machine-1")

	assert.Equal(t, "machine-1", protoRequest.GetMachineId().GetId())
	assert.Equal(t, corev1.MachineValidationOnDemandRequest_Start, protoRequest.GetAction())
	assert.Equal(t, request.Tags, protoRequest.GetTags())
	assert.Equal(t, request.AllowedTests, protoRequest.GetAllowedTests())
	assert.True(t, protoRequest.GetRunUnverfiedTests())
	assert.Equal(t, request.Contexts, protoRequest.GetContexts())
}

func TestAPIMachineValidationRunCreateRequestValidate(t *testing.T) {
	tests := []struct {
		name    string
		request APIMachineValidationRunCreateRequest
		wantErr bool
	}{
		{name: "empty options accepted"},
		{
			name: "filters accepted",
			request: APIMachineValidationRunCreateRequest{
				Tags:         []string{"history"},
				AllowedTests: []string{"gpu_bandwidth"},
				Contexts:     []string{"OnDemand"},
			},
		},
		{name: "empty tag rejected", request: APIMachineValidationRunCreateRequest{Tags: []string{""}}, wantErr: true},
		{name: "empty allowed test rejected", request: APIMachineValidationRunCreateRequest{AllowedTests: []string{""}}, wantErr: true},
		{name: "empty context rejected", request: APIMachineValidationRunCreateRequest{Contexts: []string{""}}, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)
		})
	}
}

func TestNewAPIMachineValidationRunFromOnDemandResponse(t *testing.T) {
	context := "OnDemand"
	tests := []struct {
		name     string
		response *corev1.MachineValidationOnDemandResponse
		want     *APIMachineValidationRun
	}{
		{
			name: "run populated",
			response: &corev1.MachineValidationOnDemandResponse{
				ValidationId: &corev1.MachineValidationId{Value: "validation-1"},
				Run: &corev1.MachineValidationRun{
					ValidationId: &corev1.MachineValidationId{Value: "validation-1"},
					MachineId:    &corev1.MachineId{Id: "machine-1"},
					Name:         "Test_machine-1",
					Context:      &context,
					Status: &corev1.MachineValidationStatus{
						Total:          math.MaxUint32,
						CompletedTests: math.MaxUint32,
					},
					DurationToComplete: &durationpb.Duration{Seconds: 1 << 32},
				},
			},
			want: &APIMachineValidationRun{
				ValidationID: "validation-1",
				MachineID:    "machine-1",
				Name:         "Test_machine-1",
				Context:      "OnDemand",
				Status: APIMachineValidationStatus{
					Total:     math.MaxUint32,
					Completed: math.MaxUint32,
				},
				DurationToCompleteSecs: 1 << 32,
			},
		},
		{
			name: "run unset falls back to validation ID",
			response: &corev1.MachineValidationOnDemandResponse{
				ValidationId: &corev1.MachineValidationId{Value: "validation-1"},
			},
			want: &APIMachineValidationRun{ValidationID: "validation-1"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, NewAPIMachineValidationRunFromOnDemandResponse(tt.response))
		})
	}
}
