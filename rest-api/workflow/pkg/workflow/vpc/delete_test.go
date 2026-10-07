// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpc

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	vpcActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
)

type DeleteVpcTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *DeleteVpcTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *DeleteVpcTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *DeleteVpcTestSuite) Test_DeleteVPCWorkflow_Success() {
	var vpcManager vpcActivity.ManageVPC

	vpcID := uuid.New()
	request := &corev1.VpcDeletionRequest{
		Id: &corev1.VpcId{Value: vpcID.String()},
	}

	// Mock DeleteVpcOnSite activity
	s.env.RegisterActivity(vpcManager.DeleteVpcOnSite)
	s.env.OnActivity(vpcManager.DeleteVpcOnSite, mock.Anything, request).Return(nil)

	// execute DeleteVpcByID workflow
	s.env.ExecuteWorkflow(DeleteVpcByID, vpcID)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *DeleteVpcTestSuite) Test_DeleteVPCWorkflow_ActivityFails() {
	var vpcManager vpcActivity.ManageVPC

	vpcID := uuid.New()
	request := &corev1.VpcDeletionRequest{
		Id: &corev1.VpcId{Value: vpcID.String()},
	}

	// Mock DeleteVpcOnSite activity failure
	s.env.RegisterActivity(vpcManager.DeleteVpcOnSite)
	s.env.OnActivity(vpcManager.DeleteVpcOnSite, mock.Anything, request).Return(errors.New("DeleteVpcOnSite Failure"))

	// execute DeleteVpcByID workflow
	s.env.ExecuteWorkflow(DeleteVpcByID, vpcID)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("DeleteVpcOnSite Failure", applicationErr.Error())
}

func TestDeleteVpcSuite(t *testing.T) {
	suite.Run(t, new(DeleteVpcTestSuite))
}
