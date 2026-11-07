// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operations

import (
	"time"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

type OperationOptions struct {
	Timeout time.Duration
}

var (
	defaultOperationOptions = map[taskcommon.TaskType]OperationOptions{
		taskcommon.TaskTypePowerControl: {
			Timeout: 60 * time.Minute,
		},
		taskcommon.TaskTypeFirmwareControl: {
			Timeout: 60 * time.Minute,
		},
		taskcommon.TaskTypeInjectExpectation: {
			Timeout: 60 * time.Minute,
		},
		taskcommon.TaskTypeBringUp: {
			Timeout: 120 * time.Minute,
		},
		taskcommon.TaskTypeDecommission: {
			Timeout: 4 * time.Hour,
		},
	}
)

func GetOperationOptions(typ taskcommon.TaskType) OperationOptions {
	if opt, ok := defaultOperationOptions[typ]; ok {
		return opt
	}
	return OperationOptions{
		Timeout: 0,
	}
}
