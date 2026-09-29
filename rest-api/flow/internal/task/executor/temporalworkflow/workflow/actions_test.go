// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"testing"

	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

func TestActionBatchCount(t *testing.T) {
	tests := []struct {
		name   string
		action string
		want   int
	}{
		{
			name:   "component action is partitioned",
			action: operationrules.ActionGetPowerStatus,
			want:   3,
		},
		{
			name:   "step-wide action executes once",
			action: operationrules.ActionVerifyReachability,
			want:   1,
		},
		{
			name:   "group validation executes once",
			action: operationrules.ActionVerifyFirmwareConsistency,
			want:   1,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.want, actionBatchCount(
				operationrules.ActionConfig{Name: tc.action}, 2, 5,
			))
		})
	}
}

// TestExtractOverrideReadinessCheck covers every branch of the
// extractOverrideReadinessCheck helper. The helper bridges the parent /
// sub-action boundary inside the rule engine: a parent task carries its
// OverrideReadinessCheck flag through to dynamically synthesised
// PowerControl, FirmwareControl, and BringUp sub-actions. The helper is
// intentionally type-agnostic — the workflow runtime hands it either a
// concrete TaskInfo, a pointer to one, or a JSON-deserialised
// map[string]any from a child-workflow boundary — so all of those shapes
// must extract the flag, and an unrecognised or nil input must fall back
// to false to keep the readiness gate engaged by default.
func TestExtractOverrideReadinessCheck(t *testing.T) {
	t.Run("nil returns false", func(t *testing.T) {
		assert.False(t, extractOverrideReadinessCheck(nil))
	})

	t.Run("nil typed pointers return false", func(t *testing.T) {
		var pc *operations.PowerControlTaskInfo
		var fw *operations.FirmwareControlTaskInfo
		var bu *operations.BringUpTaskInfo
		assert.False(t, extractOverrideReadinessCheck(pc))
		assert.False(t, extractOverrideReadinessCheck(fw))
		assert.False(t, extractOverrideReadinessCheck(bu))
	})

	t.Run("PowerControlTaskInfo pointer", func(t *testing.T) {
		info := &operations.PowerControlTaskInfo{OverrideReadinessCheck: true}
		assert.True(t, extractOverrideReadinessCheck(info))
		info.OverrideReadinessCheck = false
		assert.False(t, extractOverrideReadinessCheck(info))
	})

	t.Run("PowerControlTaskInfo value", func(t *testing.T) {
		info := operations.PowerControlTaskInfo{OverrideReadinessCheck: true}
		assert.True(t, extractOverrideReadinessCheck(info))
	})

	t.Run("FirmwareControlTaskInfo pointer and value", func(t *testing.T) {
		assert.True(t, extractOverrideReadinessCheck(&operations.FirmwareControlTaskInfo{
			OverrideReadinessCheck: true,
		}))
		assert.True(t, extractOverrideReadinessCheck(operations.FirmwareControlTaskInfo{
			OverrideReadinessCheck: true,
		}))
	})

	t.Run("BringUpTaskInfo pointer and value", func(t *testing.T) {
		assert.True(t, extractOverrideReadinessCheck(&operations.BringUpTaskInfo{
			OverrideReadinessCheck: true,
		}))
		assert.True(t, extractOverrideReadinessCheck(operations.BringUpTaskInfo{
			OverrideReadinessCheck: true,
		}))
	})

	t.Run("map shape from child-workflow JSON round-trip", func(t *testing.T) {
		// Temporal serialises child-workflow arguments through JSON; on
		// receipt the typed TaskInfo struct degrades to a map[string]any
		// keyed by JSON tag. The helper must still recover the flag.
		assert.True(t, extractOverrideReadinessCheck(map[string]any{
			"override_readiness_check": true,
		}))
		assert.False(t, extractOverrideReadinessCheck(map[string]any{
			"override_readiness_check": false,
		}))
		assert.False(t, extractOverrideReadinessCheck(map[string]any{
			"some_other_key": "value",
		}))
	})

	t.Run("unrecognised struct falls through JSON probe", func(t *testing.T) {
		// Anonymous struct with the matching JSON tag should still
		// be readable via the marshal/unmarshal fallback.
		type customInfo struct {
			OverrideReadinessCheck bool `json:"override_readiness_check"`
		}
		assert.True(t, extractOverrideReadinessCheck(customInfo{OverrideReadinessCheck: true}))
	})

	t.Run("non-marshalable value returns false", func(t *testing.T) {
		// A channel is not JSON-marshalable; the helper must return
		// false rather than panic.
		assert.False(t, extractOverrideReadinessCheck(make(chan int)))
	})
}
