// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestTargetResolutionMode_Validate(t *testing.T) {
	for _, tc := range []struct {
		name    string
		mode    TargetResolutionMode
		want    flowv1.TargetResolutionMode
		invalid bool
	}{
		{"empty explicit value", "", 0, true}, {"strict", TargetResolutionStrict, 1, false}, {"best effort", TargetResolutionBestEffort, 2, false}, {"unknown", "AllowPartial", 0, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.mode.Validate()
			if tc.invalid {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Equal(t, tc.want, tc.mode.ToProto())
		})
	}
}

func TestAPITaskReportV1_TargetResolution(t *testing.T) {
	var rep APITaskReportV1
	require.NoError(t, json.Unmarshal([]byte(`{"version":1,"stages":[],"target_resolution":{"mode":"BestEffort","selected":2,"submitted":1,"skipped":[{"component_id":"550e8400-e29b-41d4-a716-446655440000","component_type":"Compute","slot_id":9,"tray_index":2,"host_id":1,"reason":"unaddressable_target"}]}}`), &rep))
	require.NotNil(t, rep.TargetResolution)
	require.Equal(t, 2, rep.TargetResolution.Selected)
	raw, err := json.Marshal(rep)
	require.NoError(t, err)
	var wire map[string]any
	require.NoError(t, json.Unmarshal(raw, &wire))
	resolution := wire["targetResolution"].(map[string]any)
	skipped := resolution["skipped"].([]any)[0].(map[string]any)
	require.Equal(t, float64(9), skipped["slotId"])
	require.NotContains(t, skipped, "slot_id")
	// Reused decoders must clear fields absent from legacy reports.
	require.NoError(t, json.Unmarshal([]byte(`{"version":1,"stages":[]}`), &rep))
	require.Nil(t, rep.TargetResolution)
	raw, err = json.Marshal(rep)
	require.NoError(t, err)
	require.Contains(t, string(raw), `"targetResolution":null`)
}

func TestAPIUpdatePowerStateRequest_TargetResolutionMode(t *testing.T) {
	for _, tc := range []struct {
		name, body string
		invalid    bool
	}{
		{"omitted", `{"siteId":"site","state":"On"}`, false},
		{"strict", `{"siteId":"site","state":"On","targetResolutionMode":"Strict"}`, false},
		{"best effort", `{"siteId":"site","state":"On","targetResolutionMode":"BestEffort"}`, false},
		{"empty", `{"siteId":"site","state":"On","targetResolutionMode":""}`, true},
		{"unknown", `{"siteId":"site","state":"On","targetResolutionMode":"AllowPartial"}`, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var req APIUpdatePowerStateRequest
			require.NoError(t, json.Unmarshal([]byte(tc.body), &req))
			err := req.Validate()
			if tc.invalid {
				require.ErrorContains(t, err, "targetResolutionMode")
				return
			}
			require.NoError(t, err)
		})
	}
}
