// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package subnet

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	subnetActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
)

type DeleteSubnetTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *DeleteSubnetTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *DeleteSubnetTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *DeleteSubnetTestSuite) Test_DeleteSubnetWorkflow_Success() {
	var subnetManager subnetActivity.ManageSubnet

	subnetID := uuid.New()
	request := &corev1.NetworkSegmentDeletionRequest{
		Id: &corev1.NetworkSegmentId{Value: subnetID.String()},
	}

	// Mock DeleteSubnetOnSite activity
	s.env.RegisterActivity(subnetManager.DeleteSubnetOnSite)
	s.env.OnActivity(subnetManager.DeleteSubnetOnSite, mock.Anything, request).Return(nil)

	// execute DeleteSubnetByID workflow
	s.env.ExecuteWorkflow(DeleteSubnetByID, subnetID)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *DeleteSubnetTestSuite) Test_DeleteSubnetWorkflow_ActivityFails() {
	var subnetManager subnetActivity.ManageSubnet

	subnetID := uuid.New()

	request := &corev1.NetworkSegmentDeletionRequest{
		Id: &corev1.NetworkSegmentId{Value: subnetID.String()},
	}

	// Mock DeleteSubnetOnSite activity failure
	s.env.RegisterActivity(subnetManager.DeleteSubnetOnSite)
	s.env.OnActivity(subnetManager.DeleteSubnetOnSite, mock.Anything, request).Return(errors.New("DeleteSubnetOnSite Failure"))

	// execute DeleteSubnetByID workflow
	s.env.ExecuteWorkflow(DeleteSubnetByID, subnetID)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("DeleteSubnetOnSite Failure", applicationErr.Error())
}

func TestDeleteSubnetSuite(t *testing.T) {
	suite.Run(t, new(DeleteSubnetTestSuite))
}
