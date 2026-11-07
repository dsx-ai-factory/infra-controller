// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operations

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func TestSummaryFromWrapper(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		wrapper operation.Wrapper
		want    string
	}{
		{
			name: "power on",
			wrapper: operation.Wrapper{
				Type: taskcommon.TaskTypePowerControl,
				Code: taskcommon.OpCodePowerControlPowerOn,
			},
			want: "Power On",
		},
		{
			name: "forced power off",
			wrapper: operation.Wrapper{
				Type: taskcommon.TaskTypePowerControl,
				Code: taskcommon.OpCodePowerControlForcePowerOff,
			},
			want: "Power Off (forced)",
		},
		{
			name: "firmware upgrade with version",
			wrapper: operation.Wrapper{
				Type: taskcommon.TaskTypeFirmwareControl,
				Code: taskcommon.OpCodeFirmwareControlUpgrade,
				Info: json.RawMessage(`{"target_version":"2.3.1"}`),
			},
			want: "Upgrade Firmware to 2.3.1",
		},
		{
			name: "ingest",
			wrapper: operation.Wrapper{
				Type: taskcommon.TaskTypeBringUp,
				Code: taskcommon.OpCodeIngest,
			},
			want: "Ingest",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			got, err := SummaryFromWrapper(tt.wrapper)
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)
		})
	}
}
