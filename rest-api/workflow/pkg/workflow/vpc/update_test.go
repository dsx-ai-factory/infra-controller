// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpc

import (
	"errors"
	"testing"

	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	vpcActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/vpc"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type UpdateVpcTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateVpcTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateVpcTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateVpcTestSuite) Test_UpdateVpcInventory_Success() {
	var vpcManager vpcActivity.ManageVpc
	var lifecycleMetricsManager vpcActivity.ManageVpcLifecycleMetrics
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	siteID := uuid.New()
	vpcInventory := &corev1.VPCInventory{
		Vpcs: []*corev1.Vpc{
			{
				Id: &corev1.VpcId{Value: uuid.NewString()},
			},
			{
				Id: &corev1.VpcId{Value: uuid.NewString()},
			},
		},
	}

	// Mock UpdateVpcsInDB activity
	s.env.RegisterActivity(vpcManager.UpdateVpcsInDB)
	s.env.OnActivity(vpcManager.UpdateVpcsInDB, mock.Anything, siteID, mock.Anything).Return([]cwm.InventoryObjectLifecycleEvent{}, nil)

	// Mock RecordVpcStatusTransitionMetrics activity
	s.env.RegisterActivity(lifecycleMetricsManager.RecordVpcStatusTransitionMetrics)
	s.env.OnActivity(lifecycleMetricsManager.RecordVpcStatusTransitionMetrics, mock.Anything, siteID, mock.Anything).Return(nil)

	// Mock RecordLatency activity
	s.env.RegisterActivity(inventoryMetricsManager.RecordLatency)
	s.env.OnActivity(inventoryMetricsManager.RecordLatency, mock.Anything, siteID, "UpdateVpcInventory", false, mock.Anything).Return(nil)

	// execute UpdateVpcInventory workflow
	s.env.ExecuteWorkflow(UpdateVpcInventory, siteID.String(), vpcInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateVpcTestSuite) Test_UpdateVpcInventory_ActivityFails() {
	var vpcManager vpcActivity.ManageVpc

	siteID := uuid.New()
	vpcInventory := &corev1.VPCInventory{
		Vpcs: []*corev1.Vpc{
			{
				Id: &corev1.VpcId{Value: uuid.NewString()},
			},
			{
				Id: &corev1.VpcId{Value: uuid.NewString()},
			},
		},
	}

	// Mock UpdateVpcsInDB activity failure
	s.env.RegisterActivity(vpcManager.UpdateVpcsInDB)
	s.env.OnActivity(vpcManager.UpdateVpcsInDB, mock.Anything, siteID, mock.Anything).Return([]cwm.InventoryObjectLifecycleEvent{}, errors.New("UpdateVpcInventory Failure"))

	// execute UpdateVPCStatus workflow
	s.env.ExecuteWorkflow(UpdateVpcInventory, siteID.String(), vpcInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateVpcInventory Failure", applicationErr.Error())
}

func TestUpdateVpcSuite(t *testing.T) {
	suite.Run(t, new(UpdateVpcTestSuite))
}
