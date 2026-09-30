// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"context"
	"errors"
	"testing"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
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
		budget        time.Duration
	}{
		{"success tolerates metrics failure", corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, nil, false, nil, errors.New("metrics unavailable"), 3 * time.Minute},
		{"transient failure recovers", corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, errors.New("lock contention"), true, nil, nil, 3 * time.Minute},
		{"retries are bounded", corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, errors.New("database unavailable"), true, errors.New("database unavailable"), nil, 3 * time.Minute},
		{"invalid page does not retry", corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, temporal.NewNonRetryableApplicationError("invalid page", "InvalidSitePrefixInventory", nil), false, errors.New("invalid page"), nil, 3 * time.Minute},
		{"diagnostic does not retry", corev1.InventoryStatus_INVENTORY_STATUS_FAILED, errors.New("diagnostic failure"), false, errors.New("diagnostic failure"), nil, 30 * time.Second},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			var manager sitePrefixActivity.ManageSitePrefix
			var metrics cwm.ManageInventoryMetrics
			env.RegisterActivity(manager.UpdateSitePrefixesInDB)
			env.RegisterActivity(metrics.RecordLatency)
			env.SetOnActivityStartedListener(func(info *activity.Info, _ context.Context, _ converter.EncodedValues) {
				if info.ActivityType.Name == "RecordLatency" {
					require.Equal(t, 5*time.Second, info.ScheduleToCloseTimeout)
					require.Equal(t, 5*time.Second, info.StartToCloseTimeout)
				} else if tt.status == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
					require.Equal(t, 15*time.Second, info.ScheduleToCloseTimeout)
					require.Equal(t, 15*time.Second, info.StartToCloseTimeout)
				} else {
					require.Equal(t, 140*time.Second, info.ScheduleToCloseTimeout)
					require.Equal(t, time.Minute, info.StartToCloseTimeout)
				}
				// These server-enforced bounds include activity queue delays and
				// retries, unlike start-to-close alone.
				require.Less(t, info.ScheduleToCloseTimeout+5*time.Second, tt.budget)
			})
			id := uuid.New()
			inventory := &corev1.SitePrefixInventory{InventoryStatus: tt.status}
			delay := 55 * time.Second
			if tt.status == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
				delay = 14 * time.Second
			}
			env.OnActivity(manager.UpdateSitePrefixesInDB, mock.Anything, id, inventory).After(delay).Return(tt.firstErr).Once()
			if tt.secondAttempt {
				env.OnActivity(manager.UpdateSitePrefixesInDB, mock.Anything, id, inventory).After(delay).Return(tt.finalErr).Once()
			}
			env.OnActivity(metrics.RecordLatency, mock.Anything, id, "UpdateSitePrefixInventory", tt.finalErr != nil, mock.Anything).After(4 * time.Second).Return(tt.metricsErr).Once()
			start := env.Now()
			env.ExecuteWorkflow(UpdateSitePrefixInventory, id.String(), inventory)
			require.True(t, env.IsWorkflowCompleted())
			if tt.finalErr == nil {
				require.NoError(t, env.GetWorkflowError())
			} else {
				require.ErrorContains(t, env.GetWorkflowError(), tt.finalErr.Error())
			}
			require.Less(t, env.Now().Sub(start), tt.budget)
			env.AssertExpectations(t)
		})
	}
}
