// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package componentmanager

import (
	"context"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
)

// ComponentResetter separates the one-shot mutation from retryable observations.
// Each call operates on exactly one switch so partial failures retain attribution.
type ComponentResetter interface {
	ResetNVOS(context.Context, common.Target) error
	NVOSResetCompleted(context.Context, common.Target) (bool, error)
}
