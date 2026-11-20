// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestAPITaskReportV1_UnmarshalJSON(t *testing.T) {
	var rep APITaskReportV1
	require.NoError(t, json.Unmarshal([]byte(`{"version":1,"stages":[{"number":1,"status":"failed","steps":[{"component_type":"Compute","status":"failed","total_components":2,"targets":[{"identifier":"mac-1","identifier_type":"mac_address","status":"unknown","error":"operation timed out"},{"identifier":"mac-2","identifier_type":"mac_address","status":"skipped","error":"preceding stage failed"}]}]}],"target_resolution":{"execution_mode":"BestEffort","selected":2,"submitted":1,"skipped":[{"component_id":"550e8400-e29b-41d4-a716-446655440000","component_type":"Compute","slot_id":9,"tray_index":2,"host_id":1,"reason":"unaddressable_target"}]}}`), &rep))
	require.Len(t, rep.Stages[0].Steps[0].Targets, 2)
	require.Equal(t, "unknown", rep.Stages[0].Steps[0].Targets[0].Status)
	require.Equal(t, "operation timed out", rep.Stages[0].Steps[0].Targets[0].Error)
	require.Equal(t, "mac_address", rep.Stages[0].Steps[0].Targets[0].IdentifierType)
	require.NotNil(t, rep.TargetResolution)
	require.Equal(t, 2, rep.TargetResolution.Selected)
	require.Equal(t, "BestEffort", rep.TargetResolution.ExecutionMode)
	raw, err := json.Marshal(rep)
	require.NoError(t, err)
	var wire map[string]any
	require.NoError(t, json.Unmarshal(raw, &wire))
	resolution := wire["targetResolution"].(map[string]any)
	require.Equal(t, "BestEffort", resolution["executionMode"])
	require.NotContains(t, resolution, "mode")
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
