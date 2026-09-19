// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrackgroup

import (
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	metrics "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	activity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/expectedrackgroup"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/testsuite"
)

func TestUpdateExpectedRackGroupInventoryReconcilesSnapshot(t *testing.T) {
	env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
	siteID := uuid.New()
	inventory := &corev1.ExpectedRackGroupInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		ExpectedRackGroups: []*corev1.ExpectedRackGroup{{
			RackGroupId: &corev1.RackGroupId{Id: "nvl5-gp1-jhb01"},
			Topology:    "gb200-nvl72",
			RackIds:     []*corev1.RackId{{Id: "rack-01"}},
		}},
	}
	var manager activity.ManageExpectedRackGroup
	var metricsManager metrics.ManageInventoryMetrics
	env.RegisterActivity(manager.UpdateExpectedRackGroupsInDB)
	env.RegisterActivity(metricsManager.RecordLatency)
	env.OnActivity(manager.UpdateExpectedRackGroupsInDB, mock.Anything, siteID, inventory).Return(nil).Once()
	env.OnActivity(metricsManager.RecordLatency, mock.Anything, mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil).Once()

	env.ExecuteWorkflow(UpdateExpectedRackGroupInventory, siteID.String(), inventory)
	require.True(t, env.IsWorkflowCompleted())
	require.NoError(t, env.GetWorkflowError())
	env.AssertExpectations(t)
}
