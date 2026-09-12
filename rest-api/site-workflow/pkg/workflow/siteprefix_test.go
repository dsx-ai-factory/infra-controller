// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"

	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/testsuite"
)

func TestDiscoverSitePrefixInventory(t *testing.T) {
	tests := []struct {
		name    string
		actErr  error
		wantErr bool
	}{
		{
			name: "activity succeeds",
		},
		{
			name:    "activity fails",
			actErr:  errors.New("Site Controller communication error"),
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			var inventoryManager iActivity.ManageSitePrefixInventory
			env.RegisterActivity(inventoryManager.DiscoverSitePrefixInventory)
			env.OnActivity(inventoryManager.DiscoverSitePrefixInventory, mock.Anything).Return(tt.actErr)

			env.ExecuteWorkflow(DiscoverSitePrefixInventory)

			require.True(t, env.IsWorkflowCompleted())
			if tt.wantErr {
				require.ErrorContains(t, env.GetWorkflowError(), tt.actErr.Error())
			} else {
				require.NoError(t, env.GetWorkflowError())
			}
			env.AssertExpectations(t)
		})
	}
}
