// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"context"
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	siteActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

func TestDeleteSuccessfulInventoryWorkflows(t *testing.T) {
	tests := []struct {
		name        string
		activityErr error
		wantCalls   int
	}{
		{
			name:      "activity success completes the workflow",
			wantCalls: 1,
		},
		{
			name:        "activity failure is retried before it fails the workflow",
			activityErr: errors.New("failed to start batch job"),
			wantCalls:   3,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()

			var retentionManager siteActivity.ManageInventoryRetention
			env.RegisterActivity(retentionManager.StartSuccessfulInventoryWorkflowDeletion)
			env.OnActivity(retentionManager.StartSuccessfulInventoryWorkflowDeletion, mock.Anything).Return(tt.activityErr)

			env.ExecuteWorkflow(DeleteSuccessfulInventoryWorkflows)

			require.True(t, env.IsWorkflowCompleted())
			if tt.activityErr != nil {
				var applicationErr *temporal.ApplicationError
				require.ErrorAs(t, env.GetWorkflowError(), &applicationErr)
				assert.Equal(t, tt.activityErr.Error(), applicationErr.Error())
			} else {
				assert.NoError(t, env.GetWorkflowError())
			}
			env.AssertActivityNumberOfCalls(t, "StartSuccessfulInventoryWorkflowDeletion", tt.wantCalls)
		})
	}
}

func TestExecuteDeleteSuccessfulInventoryWorkflows(t *testing.T) {
	executeErr := errors.New("failed to start workflow")

	tests := []struct {
		name       string
		executeErr error
	}{
		{
			name: "cron is started on the Site task queue",
		},
		{
			name:       "start error is returned",
			executeErr: executeErr,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// A fixed ID is what makes every Site worker replica find the same cron
			wantOptions := client.StartWorkflowOptions{
				ID:           "delete-successful-inventory-workflows",
				CronSchedule: "@every 1h",
				TaskQueue:    queue.SiteTaskQueue,
			}

			var wrun *tmocks.WorkflowRun
			if tt.executeErr == nil {
				wrun = tmocks.NewWorkflowRun(t)
				wrun.On("GetID").Return(wantOptions.ID)
			}

			tc := tmocks.NewClient(t)
			tc.On("ExecuteWorkflow", mock.Anything, wantOptions, mock.Anything).Return(wrun, tt.executeErr)

			wid, err := ExecuteDeleteSuccessfulInventoryWorkflows(context.Background(), tc)
			if tt.executeErr != nil {
				assert.ErrorIs(t, err, tt.executeErr)
				assert.Nil(t, wid)
				return
			}

			require.NoError(t, err)
			assert.Equal(t, wantOptions.ID, *wid)
		})
	}
}
