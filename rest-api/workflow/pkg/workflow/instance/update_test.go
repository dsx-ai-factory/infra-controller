// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instance

import (
	"errors"
	"testing"

	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	instanceActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/instance"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	"google.golang.org/protobuf/types/known/timestamppb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type UpdateInstanceTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateInstanceTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateInstanceTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateInstanceTestSuite) Test_UpdateInstanceInventory_Success() {
	var instanceManager instanceActivity.ManageInstance
	var lifecycleMetricsManager instanceActivity.ManageInstanceLifecycleMetrics
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	siteID := uuid.New()

	instanceInventory := &corev1.InstanceInventory{
		Instances: []*corev1.Instance{},
		Timestamp: timestamppb.Now(),
	}

	// Mock UpdateInstancesInDB activity
	s.env.RegisterActivity(instanceManager.UpdateInstancesInDB)
	s.env.OnActivity(instanceManager.UpdateInstancesInDB, mock.Anything, siteID, mock.Anything).Return([]cwm.InventoryObjectLifecycleEvent{}, nil)

	// Mock RecordInstanceStatusTransitionMetrics activity
	s.env.RegisterActivity(lifecycleMetricsManager.RecordInstanceStatusTransitionMetrics)
	s.env.OnActivity(lifecycleMetricsManager.RecordInstanceStatusTransitionMetrics, mock.Anything, siteID, mock.Anything).Return(nil)

	// Mock RecordLatency activity
	s.env.RegisterActivity(inventoryMetricsManager.RecordLatency)
	s.env.OnActivity(inventoryMetricsManager.RecordLatency, mock.Anything, siteID, "UpdateInstanceInventory", false, mock.Anything).Return(nil)

	// execute UpdateInstanceInventory workflow
	s.env.ExecuteWorkflow(UpdateInstanceInventory, siteID.String(), instanceInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateInstanceTestSuite) Test_UpdateInstanceInventory_ActivityFails() {
	var instanceManager instanceActivity.ManageInstance

	siteID := uuid.New()

	instanceInventory := &corev1.InstanceInventory{
		Instances: []*corev1.Instance{},
		Timestamp: timestamppb.Now(),
	}

	// Mock UpdateInstancesInDB activity failure
	s.env.RegisterActivity(instanceManager.UpdateInstancesInDB)
	s.env.OnActivity(instanceManager.UpdateInstancesInDB, mock.Anything, mock.Anything, mock.Anything).Return([]cwm.InventoryObjectLifecycleEvent{}, errors.New("UpdateInstanceInventory Failure"))

	// execute UpdateInstanceInventory workflow
	s.env.ExecuteWorkflow(UpdateInstanceInventory, siteID.String(), instanceInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateInstanceInventory Failure", applicationErr.Error())
}

func TestUpdateInstanceInfoSuite(t *testing.T) {
	suite.Run(t, new(UpdateInstanceTestSuite))
}
