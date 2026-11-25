// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package leakage

import (
	"encoding/json"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

// PreserveShutdownTask retains accepted direct shutdown work during leakage
// preemption. An overlapping response may cover fewer devices, so replacing the
// entire existing task could abandon shutdown work on its remaining targets.
func PreserveShutdownTask(op operation.Wrapper) bool {
	if op.Type != taskcommon.TaskTypePowerControl {
		return false
	}
	var info operations.PowerControlTaskInfo
	return json.Unmarshal(op.Info, &info) == nil && info.Operation == operations.PowerOperationForcePowerOff && info.OverrideReadinessCheck && info.Validate() == nil && op.Code == info.CodeString()
}
