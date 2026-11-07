// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instancetype

import (
	"errors"
	"testing"

	instanceTypeActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/instancetype"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type UpdateInstanceTypeTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateInstanceTypeTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateInstanceTypeTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateInstanceTypeTestSuite) Test_UpdateInstanceTypeInventory_Success() {

	var instanceTypeManager instanceTypeActivity.ManageInstanceType

	siteID := uuid.New()
	instanceTypeInventory := &corev1.InstanceTypeInventory{
		InstanceTypes: []*corev1.InstanceType{
			{
				Id: uuid.NewString(),
			},
			{
				Id: uuid.NewString(),
			},
		},
	}

	// Mock UpdateInstanceTypeViaSiteAgent activity
	s.env.RegisterActivity(instanceTypeManager.UpdateInstanceTypesInDB)
	s.env.OnActivity(instanceTypeManager.UpdateInstanceTypesInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateInstanceTypeInventory workflow
	s.env.ExecuteWorkflow(UpdateInstanceTypeInventory, siteID.String(), instanceTypeInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateInstanceTypeTestSuite) Test_UpdateInstanceTypeInventory_ActivityFails() {

	var instanceTypeManager instanceTypeActivity.ManageInstanceType

	siteID := uuid.New()
	instanceTypeInventory := &corev1.InstanceTypeInventory{
		InstanceTypes: []*corev1.InstanceType{
			{
				Id: uuid.NewString(),
			},
			{
				Id: uuid.NewString(),
			},
		},
	}

	// Mock UpdateInstanceTypesViaSiteAgent activity failure
	s.env.RegisterActivity(instanceTypeManager.UpdateInstanceTypesInDB)
	s.env.OnActivity(instanceTypeManager.UpdateInstanceTypesInDB, mock.Anything, mock.Anything, mock.Anything).Return(errors.New("UpdateInstanceTypeInventory Failure"))

	// execute UpdateInstanceTypeStatus workflow
	s.env.ExecuteWorkflow(UpdateInstanceTypeInventory, siteID.String(), instanceTypeInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateInstanceTypeInventory Failure", applicationErr.Error())
}

func TestUpdateInstanceTypeSuite(t *testing.T) {
	suite.Run(t, new(UpdateInstanceTypeTestSuite))
}
