// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package subnet

import (
	"errors"
	"testing"

	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	subnetActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/subnet"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	"google.golang.org/protobuf/types/known/timestamppb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type UpdateSubnetTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateSubnetTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateSubnetTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateSubnetTestSuite) Test_UpdateSubnetInventory_Success() {
	var subnetManager subnetActivity.ManageSubnet
	var lifecycleMetricsManager subnetActivity.ManageSubnetLifecycleMetrics
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	siteID := uuid.New()

	subnetInventory := &corev1.SubnetInventory{
		Segments:  []*corev1.NetworkSegment{},
		Timestamp: timestamppb.Now(),
	}

	// Mock UpdateSubnetsInDB activity
	s.env.RegisterActivity(subnetManager.UpdateSubnetsInDB)
	s.env.OnActivity(subnetManager.UpdateSubnetsInDB, mock.Anything, siteID, mock.Anything).Return([]cwm.InventoryObjectLifecycleEvent{}, nil)

	// Mock RecordSubnetStatusTransitionMetrics activity
	s.env.RegisterActivity(lifecycleMetricsManager.RecordSubnetStatusTransitionMetrics)
	s.env.OnActivity(lifecycleMetricsManager.RecordSubnetStatusTransitionMetrics, mock.Anything, siteID, mock.Anything).Return(nil)

	// Mock RecordLatency activity
	s.env.RegisterActivity(inventoryMetricsManager.RecordLatency)
	s.env.OnActivity(inventoryMetricsManager.RecordLatency, mock.Anything, siteID, "UpdateSubnetInventory", false, mock.Anything).Return(nil)

	// Execute UpdateSubnetInventory workflow
	s.env.ExecuteWorkflow(UpdateSubnetInventory, siteID.String(), subnetInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateSubnetTestSuite) Test_UpdateSubnetInventory_ActivityFails() {
	var subnetManager subnetActivity.ManageSubnet

	siteID := uuid.New()

	subnetInventory := &corev1.SubnetInventory{
		Segments:  []*corev1.NetworkSegment{},
		Timestamp: timestamppb.Now(),
	}

	// Mock UpdateSubnetsInDB activity
	s.env.RegisterActivity(subnetManager.UpdateSubnetsInDB)
	s.env.OnActivity(subnetManager.UpdateSubnetsInDB, mock.Anything, siteID, mock.Anything).Return([]cwm.InventoryObjectLifecycleEvent{}, errors.New("UpdateSubnetInventory Failure"))

	// Execute UpdateSubnetInventory workflow
	s.env.ExecuteWorkflow(UpdateSubnetInventory, siteID.String(), subnetInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateSubnetInventory Failure", applicationErr.Error())
}

func TestUpdateSubnetInfoSuite(t *testing.T) {
	suite.Run(t, new(UpdateSubnetTestSuite))
}
