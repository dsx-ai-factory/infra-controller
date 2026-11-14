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
	require.NoError(t, json.Unmarshal([]byte(`{"version":1,"stages":[],"target_resolution":{"skip_unaddressable_targets":true,"selected":2,"submitted":1,"skipped":[{"component_id":"550e8400-e29b-41d4-a716-446655440000","component_type":"Compute","slot_id":9,"tray_index":2,"host_id":1,"reason":"unaddressable_target"}]}}`), &rep))
	require.NotNil(t, rep.TargetResolution)
	require.Equal(t, 2, rep.TargetResolution.Selected)
	require.True(t, rep.TargetResolution.SkipUnaddressableTargets)
	raw, err := json.Marshal(rep)
	require.NoError(t, err)
	var wire map[string]any
	require.NoError(t, json.Unmarshal(raw, &wire))
	resolution := wire["targetResolution"].(map[string]any)
	require.Equal(t, true, resolution["skipUnaddressableTargets"])
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
