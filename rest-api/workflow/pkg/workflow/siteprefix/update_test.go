// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"context"
	"errors"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	sitePrefixActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/siteprefix"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/converter"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
)

func TestUpdateSitePrefixInventory(t *testing.T) {
	tests := []struct {
		name          string
		status        corev1.InventoryStatus
		firstErr      error
		secondAttempt bool
		finalErr      error
		metricsErr    error
	}{
		{"success tolerates metrics failure", corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, nil, false, nil, errors.New("metrics unavailable")},
		{"transient failure recovers", corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, errors.New("lock contention"), true, nil, nil},
		{"retries are bounded", corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, errors.New("database unavailable"), true, errors.New("database unavailable"), nil},
		{"invalid prefix does not retry", corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, temporal.NewNonRetryableApplicationError("invalid prefix", "InvalidSitePrefixInventory", nil), false, errors.New("invalid prefix"), nil},
		{"diagnostic uses shared retries", corev1.InventoryStatus_INVENTORY_STATUS_FAILED, errors.New("diagnostic failure"), true, nil, nil},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			runTimeout := 10 * cwi.ActivityStartToCloseTimeout
			env.SetWorkflowRunTimeout(runTimeout)
			var manager sitePrefixActivity.ManageSitePrefix
			var metrics cwm.ManageInventoryMetrics
			env.RegisterActivity(manager.UpdateSitePrefixesInDB)
			env.RegisterActivity(metrics.RecordLatency)
			env.SetOnActivityStartedListener(func(info *activity.Info, _ context.Context, _ converter.EncodedValues) {
				options := cwi.ActivityOptions()
				require.Equal(t, options.StartToCloseTimeout, info.StartToCloseTimeout)
				// The test server fills an unset schedule-to-close from the workflow timeout.
				require.Equal(t, runTimeout, info.ScheduleToCloseTimeout)
			})
			id := uuid.New()
			inventory := &corev1.SitePrefixInventory{InventoryStatus: tt.status}
			env.OnActivity(manager.UpdateSitePrefixesInDB, mock.Anything, id, inventory).Return(tt.firstErr).Once()
			if tt.secondAttempt {
				env.OnActivity(manager.UpdateSitePrefixesInDB, mock.Anything, id, inventory).Return(tt.finalErr).Once()
			}
			metricsAttempts := 1
			if tt.metricsErr != nil {
				metricsAttempts = cwi.ActivityMaximumAttempts
			}
			env.OnActivity(metrics.RecordLatency, mock.Anything, id, "UpdateSitePrefixInventory", tt.finalErr != nil, mock.Anything).Return(tt.metricsErr).Times(metricsAttempts)
			env.ExecuteWorkflow(UpdateSitePrefixInventory, id.String(), inventory)
			require.True(t, env.IsWorkflowCompleted())
			if tt.finalErr == nil {
				require.NoError(t, env.GetWorkflowError())
			} else {
				require.ErrorContains(t, env.GetWorkflowError(), tt.finalErr.Error())
			}
			env.AssertExpectations(t)
		})
	}
}
