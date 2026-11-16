// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operations

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func TestComponentResetTaskInfo_Persistence(t *testing.T) {
	for _, tc := range []struct {
		name, raw, target, code string
		invalid                 bool
	}{
		{name: "missing persisted target is invalid", raw: `{}`, invalid: true},
		{name: "explicit NVOS", raw: `{"target":"nvos"}`, target: "nvos", code: taskcommon.OpCodeNVOSFactoryReset},
		{name: "SDN survives reload", raw: `{"target":"sdn"}`, target: "sdn", code: taskcommon.OpCodeSDNFactoryReset},
		{name: "invalid persisted scope", raw: `{"target":"bmc"}`, invalid: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			op, err := New(taskcommon.TaskTypeComponentReset, json.RawMessage(tc.raw))
			require.NoError(t, err)
			err = op.Validate()
			if tc.invalid {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			raw, err := op.Marshal()
			require.NoError(t, err)
			reloaded, err := New(taskcommon.TaskTypeComponentReset, raw)
			require.NoError(t, err)
			require.NoError(t, reloaded.Validate())
			require.Equal(t, tc.code, reloaded.CodeString())
			require.Equal(t, tc.target, reloaded.(*ComponentResetTaskInfo).Target)
			require.Equal(t, reloaded, reloaded.Clone())
		})
	}
}
