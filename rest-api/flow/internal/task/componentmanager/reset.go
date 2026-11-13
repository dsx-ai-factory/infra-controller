// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package componentmanager

import (
	"context"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
)

// ComponentResetter gates a rack batch, submits one switch at a time, and
// observes accepted resets without repeating the mutation.
type ComponentResetter interface {
	PrepareFactoryReset(context.Context, common.Target) error
	SubmitFactoryReset(context.Context, common.Target, string) error
	NVOSResetCompleted(context.Context, common.Target) (bool, error)
	SDNResetCompleted(context.Context, common.Target) (bool, error)
}
