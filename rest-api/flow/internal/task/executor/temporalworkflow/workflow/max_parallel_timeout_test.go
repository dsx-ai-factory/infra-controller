// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"testing"
	"time"

	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
)

func TestChildWorkflowExecutionTimeoutAccountsForBatches(t *testing.T) {
	tests := []struct {
		name           string
		maxParallel    int
		componentCount int
		want           time.Duration
	}{
		{
			name:           "unlimited uses one batch",
			maxParallel:    0,
			componentCount: 5,
			want:           3*time.Minute + 30*time.Second,
		},
		{
			name:           "limit equal to count uses one batch",
			maxParallel:    3,
			componentCount: 3,
			want:           3*time.Minute + 30*time.Second,
		},
		{
			name:           "partial final batch extends component action budgets",
			maxParallel:    2,
			componentCount: 5,
			want:           5*time.Minute + 50*time.Second,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			step := operationrules.SequenceStep{
				MaxParallel: tc.maxParallel,
				Timeout:     time.Minute,
				MainOperation: operationrules.ActionConfig{
					Name: operationrules.ActionGetPowerStatus,
				},
				PreOperation: []operationrules.ActionConfig{
					{
						Name:    operationrules.ActionGetPowerStatus,
						Timeout: 10 * time.Second,
					},
				},
				PostOperation: []operationrules.ActionConfig{
					{
						Name:    operationrules.ActionSleep,
						Timeout: 20 * time.Second,
					},
				},
			}

			assert.Equal(
				t,
				tc.want,
				childWorkflowExecutionTimeout(step, tc.componentCount),
			)
		})
	}
}
