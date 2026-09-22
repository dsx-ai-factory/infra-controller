// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package executor

import (
	"context"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

// NoopExecutor completes noop actions without an external side effect.
type NoopExecutor struct{}

// Execute completes a typed noop request.
func (NoopExecutor) Execute(_ context.Context, request ExecutionRequest) error {
	plan, ok := request.Plan.(*eventrule.NoopPlan)
	if !ok || plan == nil {
		return terminalError(fmt.Errorf(
			"noop executor received plan %T",
			request.Plan,
		))
	}

	return nil
}

var _ Executor = NoopExecutor{}
