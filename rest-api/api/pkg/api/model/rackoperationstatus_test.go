// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPIRack_FromProto(t *testing.T) {
	tests := []struct {
		name    string
		details *flowv1.RackOperationStatusDetails
		want    string
	}{
		{name: "older Flow has no details", want: `null`},
		{name: "unavailable counts", details: &flowv1.RackOperationStatusDetails{}, want: `null`},
		{
			name:    "empty membership retains zero counts",
			details: &flowv1.RackOperationStatusDetails{TrayCountsByType: &flowv1.TrayStatusCountsByType{}},
			want: `{"trayCountsByType":{
				"compute":{"ready":0,"inUse":0,"initializing":0,"error":0,"deleting":0,"unknown":0},
				"nvSwitch":{"ready":0,"inUse":0,"initializing":0,"error":0,"deleting":0,"unknown":0},
				"powerShelf":{"ready":0,"inUse":0,"initializing":0,"error":0,"deleting":0,"unknown":0}}}`,
		},
		{
			name: "type and state counters survive conversion",
			details: &flowv1.RackOperationStatusDetails{TrayCountsByType: &flowv1.TrayStatusCountsByType{
				Compute:    &flowv1.TrayStatusCounts{Ready: 6, InUse: 5},
				NvSwitch:   &flowv1.TrayStatusCounts{Initializing: 4, Error: 3},
				PowerShelf: &flowv1.TrayStatusCounts{Deleting: 2, Unknown: 1},
			}},
			want: `{"trayCountsByType":{
				"compute":{"ready":6,"inUse":5,"initializing":0,"error":0,"deleting":0,"unknown":0},
				"nvSwitch":{"ready":0,"inUse":0,"initializing":4,"error":3,"deleting":0,"unknown":0},
				"powerShelf":{"ready":0,"inUse":0,"initializing":0,"error":0,"deleting":2,"unknown":1}}}`,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Reuse a populated receiver to prove older responses clear stale details.
			rack := APIRack{OperationStatusDetails: &APIRackOperationStatusDetails{TrayCountsByType: APITrayStatusCountsByType{Compute: APITrayStatusCounts{Error: 99}}}}
			rack.FromProto(&flowv1.Rack{OperationStatusDetails: tt.details}, false)
			encoded, err := json.Marshal(rack)
			require.NoError(t, err)
			var fields map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(encoded, &fields))
			assert.JSONEq(t, tt.want, string(fields["operationStatusDetails"]))
		})
	}
}
