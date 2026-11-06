// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"testing"

	"github.com/stretchr/testify/require"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestTargetResolutionModeFrom(t *testing.T) {
	for _, tc := range []struct {
		value   pb.TargetResolutionMode
		want    taskcommon.TargetResolutionMode
		invalid bool
	}{
		{0, taskcommon.TargetResolutionStrict, false}, {1, taskcommon.TargetResolutionStrict, false}, {2, taskcommon.TargetResolutionBestEffort, false}, {99, "", true},
	} {
		t.Run(tc.value.String(), func(t *testing.T) {
			got, err := TargetResolutionModeFrom(tc.value)
			if tc.invalid {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Equal(t, tc.want, got)
		})
	}
}
