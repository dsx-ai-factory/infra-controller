// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"fmt"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// TargetResolutionModeFrom accepts omission as Strict and rejects unknown wire values.
func TargetResolutionModeFrom(value pb.TargetResolutionMode) (taskcommon.TargetResolutionMode, error) {
	switch value {
	case pb.TargetResolutionMode_TARGET_RESOLUTION_MODE_UNSPECIFIED, pb.TargetResolutionMode_TARGET_RESOLUTION_MODE_STRICT:
		return taskcommon.TargetResolutionStrict, nil
	case pb.TargetResolutionMode_TARGET_RESOLUTION_MODE_BEST_EFFORT:
		return taskcommon.TargetResolutionBestEffort, nil
	default:
		return "", fmt.Errorf("unknown target_resolution_mode %d", value)
	}
}
