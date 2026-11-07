// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instance

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	instanceActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
)

type DeleteInstanceTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *DeleteInstanceTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *DeleteInstanceTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *DeleteInstanceTestSuite) Test_DeleteInstanceWorkflow_Success() {
	var instanceManager instanceActivity.ManageInstance

	instanceID := uuid.New()
	request := &corev1.InstanceReleaseRequest{
		Id: &corev1.InstanceId{
			Value: instanceID.String(),
		},
	}

	// Mock DeleteInstanceOnSite activity
	s.env.RegisterActivity(instanceManager.DeleteInstanceOnSite)
	s.env.OnActivity(instanceManager.DeleteInstanceOnSite, mock.Anything, request).Return(nil)

	// execute deleteVPC workflow
	s.env.ExecuteWorkflow(DeleteInstanceByID, instanceID)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *DeleteInstanceTestSuite) Test_DeleteInstanceWorkflow_DeleteInstanceViaSiteAgentActivityFails() {
	var instanceManager instanceActivity.ManageInstance

	instanceID := uuid.New()

	request := &corev1.InstanceReleaseRequest{
		Id: &corev1.InstanceId{
			Value: instanceID.String(),
		},
	}

	// Mock DeleteInstanceOnSite activity failure
	s.env.RegisterActivity(instanceManager.DeleteInstanceOnSite)
	s.env.OnActivity(instanceManager.DeleteInstanceOnSite, mock.Anything, request).Return(errors.New("DeleteInstanceOnSite Failure"))

	// execute DeleteInstanceByID workflow
	s.env.ExecuteWorkflow(DeleteInstanceByID, instanceID)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("DeleteInstanceOnSite Failure", applicationErr.Error())
}

func TestDeleteInstanceSuite(t *testing.T) {
	suite.Run(t, new(DeleteInstanceTestSuite))
}
