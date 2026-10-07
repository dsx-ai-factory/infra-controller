// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedmachine

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	expectedMachineActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/expectedmachine"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type UpdateExpectedMachineTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateExpectedMachineTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateExpectedMachineTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateExpectedMachineTestSuite) Test_UpdateExpectedMachineInventory_Success() {
	var expectedMachineManager expectedMachineActivity.ManageExpectedMachine

	siteID := uuid.New()

	inv := &corev1.ExpectedMachineInventory{
		ExpectedMachines: []*corev1.ExpectedMachine{},
	}

	s.env.RegisterActivity(expectedMachineManager.UpdateExpectedMachinesInDB)
	s.env.OnActivity(expectedMachineManager.UpdateExpectedMachinesInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	s.env.ExecuteWorkflow(UpdateExpectedMachineInventory, siteID.String(), inv)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateExpectedMachineTestSuite) Test_UpdateExpectedMachineInventory_ActivityFails() {
	var expectedMachineManager expectedMachineActivity.ManageExpectedMachine

	siteID := uuid.New()

	inv := &corev1.ExpectedMachineInventory{
		ExpectedMachines: []*corev1.ExpectedMachine{},
	}

	s.env.RegisterActivity(expectedMachineManager.UpdateExpectedMachinesInDB)
	s.env.OnActivity(expectedMachineManager.UpdateExpectedMachinesInDB, mock.Anything, mock.Anything, mock.Anything).Return(errors.New("UpdateExpectedMachineInventory Failure"))

	s.env.ExecuteWorkflow(UpdateExpectedMachineInventory, siteID.String(), inv)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.NotNil(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateExpectedMachineInventory Failure", applicationErr.Error())
}

func TestUpdateExpectedMachineTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateExpectedMachineTestSuite))
}
