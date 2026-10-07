// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrack

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	expectedRackActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/expectedrack"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type UpdateExpectedRackTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateExpectedRackTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateExpectedRackTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateExpectedRackTestSuite) Test_UpdateExpectedRackInventory_Success() {
	var expectedRackManager expectedRackActivity.ManageExpectedRack
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	siteID := uuid.New()

	inv := &corev1.ExpectedRackInventory{
		ExpectedRacks: []*corev1.ExpectedRack{},
	}

	s.env.RegisterActivity(expectedRackManager.UpdateExpectedRacksInDB)
	s.env.RegisterActivity(inventoryMetricsManager.RecordLatency)
	s.env.OnActivity(expectedRackManager.UpdateExpectedRacksInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil)
	s.env.OnActivity(inventoryMetricsManager.RecordLatency, mock.Anything, mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil).Once()

	s.env.ExecuteWorkflow(UpdateExpectedRackInventory, siteID.String(), inv)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateExpectedRackTestSuite) Test_UpdateExpectedRackInventory_ActivityFails() {
	var expectedRackManager expectedRackActivity.ManageExpectedRack

	siteID := uuid.New()

	inv := &corev1.ExpectedRackInventory{
		ExpectedRacks: []*corev1.ExpectedRack{},
	}

	s.env.RegisterActivity(expectedRackManager.UpdateExpectedRacksInDB)
	s.env.OnActivity(expectedRackManager.UpdateExpectedRacksInDB, mock.Anything, mock.Anything, mock.Anything).Return(errors.New("UpdateExpectedRackInventory Failure"))

	s.env.ExecuteWorkflow(UpdateExpectedRackInventory, siteID.String(), inv)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.NotNil(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateExpectedRackInventory Failure", applicationErr.Error())
}

func (s *UpdateExpectedRackTestSuite) Test_UpdateExpectedRackInventory_InvalidSiteID() {
	inv := &corev1.ExpectedRackInventory{
		ExpectedRacks: []*corev1.ExpectedRack{},
	}

	// No activities registered: workflow must reject before invoking any activity.
	s.env.ExecuteWorkflow(UpdateExpectedRackInventory, "not-a-uuid", inv)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.NotNil(err)
}

func TestUpdateExpectedRackTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateExpectedRackTestSuite))
}
