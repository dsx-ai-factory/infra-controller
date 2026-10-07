// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"errors"
	"testing"
	"time"

	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	sdkactivity "go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"
)

func TestDiscoverSitePrefixInventory(t *testing.T) {
	tests := []struct {
		name         string
		actErr       error
		wantErr      bool
		wantAttempts int
	}{
		{
			name:         "activity succeeds",
			wantAttempts: 1,
		},
		{
			name:         "activity fails after one retry",
			actErr:       errors.New("Site Controller communication error"),
			wantErr:      true,
			wantAttempts: 2,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			var inventoryManager iActivity.ManageSitePrefixInventory
			env.RegisterActivity(inventoryManager.DiscoverSitePrefixInventory)
			var attemptTimeouts []time.Duration
			env.OnActivity(inventoryManager.DiscoverSitePrefixInventory, mock.Anything).Return(func(ctx context.Context) error {
				attemptTimeouts = append(attemptTimeouts, sdkactivity.GetInfo(ctx).StartToCloseTimeout)
				return tt.actErr
			})

			env.ExecuteWorkflow(DiscoverSitePrefixInventory)

			require.True(t, env.IsWorkflowCompleted())
			if tt.wantErr {
				require.ErrorContains(t, env.GetWorkflowError(), tt.actErr.Error())
			} else {
				require.NoError(t, env.GetWorkflowError())
			}
			require.Len(t, attemptTimeouts, tt.wantAttempts)
			for _, timeout := range attemptTimeouts {
				require.Equal(t, 10*time.Minute, timeout)
			}
			env.AssertExpectations(t)
		})
	}
}
