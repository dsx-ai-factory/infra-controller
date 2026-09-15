// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPIDeletionAcceptedResponse_JSON(t *testing.T) {
	t.Parallel()

	payload, err := json.Marshal(NewAPIDeletionAcceptedResponse())
	require.NoError(t, err)
	assert.JSONEq(t, `{"message":"`+DeletionRequestAcceptedMessage+`"}`, string(payload))

	var decoded APIMessageResponse
	require.NoError(t, json.Unmarshal(payload, &decoded))
	assert.Equal(t, DeletionRequestAcceptedMessage, decoded.Message)
}

func TestAPILabels_MarshalJSON(t *testing.T) {
	tests := []struct {
		name   string
		labels APILabels
		want   string
	}{
		{name: "nil", want: `{}`},
		{name: "empty", labels: APILabels{}, want: `{}`},
		{name: "populated", labels: APILabels{"env": "test", "note": "quoted \"value\""}, want: `{"env":"test","note":"quoted \"value\""}`},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			raw, err := json.Marshal(tc.labels)
			require.NoError(t, err)
			assert.JSONEq(t, tc.want, string(raw))
		})
	}
}

func TestAPIList_MarshalJSON(t *testing.T) {
	tests := []struct {
		name    string
		items   any
		want    string
		wantErr bool
	}{
		{name: "nil", items: APIList[string](nil), want: `[]`},
		{name: "empty", items: APIList[string]{}, want: `[]`},
		{name: "populated preserves order", items: APIList[string]{"DPU002", "DPU001"}, want: `["DPU002","DPU001"]`},
		{name: "unsupported element returns error", items: APIList[chan int]{make(chan int)}, wantErr: true},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			raw, err := json.Marshal(tc.items)
			if tc.wantErr {
				var unsupported *json.UnsupportedTypeError
				require.ErrorAs(t, err, &unsupported)
				return
			}
			require.NoError(t, err)
			assert.JSONEq(t, tc.want, string(raw))
		})
	}
}

// Each response must use the shared label type without omitempty. Exercise the
// constructors as well so DB and protobuf conversion paths stay wired to it.
func TestAPIResponseLabels(t *testing.T) {
	siteID := uuid.New()
	dpus := NewAPIDpuMachines([]*corev1.DpuMachine{{Machine: &corev1.Machine{}}}, APIDpuMachineProtoContext{})
	spectrumXPartition := &APISpectrumXPartition{}
	spectrumXPartition.FromDB(&cdbm.SpectrumXPartition{}, nil)
	tests := []struct {
		name     string
		response any
	}{
		{"ExpectedMachine", NewAPIExpectedMachine(&cdbm.ExpectedMachine{})},
		{"ExpectedSwitch", NewAPIExpectedSwitch(&cdbm.ExpectedSwitch{})},
		{"ExpectedPowerShelf", NewAPIExpectedPowerShelf(&cdbm.ExpectedPowerShelf{})},
		{"ExpectedRack", NewAPIExpectedRack(&cdbm.ExpectedRack{})},
		{"InstanceType", NewAPIInstanceType(&cdbm.InstanceType{SiteID: &siteID}, nil, nil, nil, nil)},
		{"InfiniBandPartition", NewAPIInfiniBandPartition(&cdbm.InfiniBandPartition{}, nil)},
		{"NetworkSecurityGroup", NewAPINetworkSecurityGroup(&cdbm.NetworkSecurityGroup{}, nil)},
		{"VPC", NewAPIVpc(cdbm.Vpc{}, nil, false)},
		{"Machine", NewAPIMachine(&cdbm.Machine{}, nil, nil, nil, nil, false, true)},
		{"Instance", NewAPIInstance(&cdbm.Instance{}, &cdbm.Site{}, nil, nil, nil, nil, nil, nil, nil)},
		{"DpuMachine", dpus[0]},
		{"SpectrumXPartition", spectrumXPartition},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			raw, err := json.Marshal(tc.response)
			require.NoError(t, err)
			var fields map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(raw, &fields))
			assert.JSONEq(t, `{}`, string(fields["labels"]))
		})
	}
}
