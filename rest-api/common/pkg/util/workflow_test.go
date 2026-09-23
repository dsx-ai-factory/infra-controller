// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
)

// TestTimeoutLadder guards the ordering the bespoke Site workflows rely on.
// grpcproxy pins the same ordering for the proxy path, so this can go when the
// last bespoke workflow has moved over.
func TestTimeoutLadder(t *testing.T) {
	cases := []struct {
		name    string
		shorter time.Duration
		longer  time.Duration
		why     string
	}{
		{
			name:    "activity finishes before the workflow expires",
			shorter: ActivityStartToCloseTimeout,
			longer:  WorkflowExecutionTimeout,
			why:     "a Site activity hands its context to the on-site gRPC call, so an activity outliving its workflow lets Core keep working on a request nothing is waiting for",
		},
		{
			name:    "workflow expires before the caller gives up",
			shorter: WorkflowExecutionTimeout,
			longer:  WorkflowContextTimeout,
			why:     "the caller must outlast the workflow so a terminal result is observed rather than the handler abandoning a live execution and rolling its transaction back",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Less(t, tc.shorter, tc.longer, tc.why)
		})
	}
}
