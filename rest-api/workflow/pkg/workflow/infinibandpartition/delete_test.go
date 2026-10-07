// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package infinibandpartition

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	ibpActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
)

type DeleteInfiniBandPartitionTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *DeleteInfiniBandPartitionTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *DeleteInfiniBandPartitionTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *DeleteInfiniBandPartitionTestSuite) Test_DeleteInfiniBandPartitionWorkflow_Success() {
	var ibpManager ibpActivity.ManageInfiniBandPartition

	ibpID := uuid.New()
	request := &corev1.IBPartitionDeletionRequest{
		Id: &corev1.IBPartitionId{Value: ibpID.String()},
	}

	// Mock DeleteInfiniBandPartitionOnSite activity
	s.env.RegisterActivity(ibpManager.DeleteInfiniBandPartitionOnSite)
	s.env.OnActivity(ibpManager.DeleteInfiniBandPartitionOnSite, mock.Anything, request).Return(nil)

	// Execute DeleteInfiniBandPartitionByID workflow
	s.env.ExecuteWorkflow(DeleteInfiniBandPartitionByID, ibpID)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *DeleteInfiniBandPartitionTestSuite) Test_DeleteInfiniBandPartitionWorkflow_ActivityFails() {
	var ibpManager ibpActivity.ManageInfiniBandPartition

	ibpID := uuid.New()
	request := &corev1.IBPartitionDeletionRequest{
		Id: &corev1.IBPartitionId{Value: ibpID.String()},
	}

	// Mock DeleteInfiniBandPartitionOnSite activity failure
	s.env.RegisterActivity(ibpManager.DeleteInfiniBandPartitionOnSite)
	s.env.OnActivity(ibpManager.DeleteInfiniBandPartitionOnSite, mock.Anything, request).Return(errors.New("DeleteInfiniBandPartitionOnSite Failure"))

	// Execute DeleteInfiniBandPartitionByID workflow
	s.env.ExecuteWorkflow(DeleteInfiniBandPartitionByID, ibpID)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("DeleteInfiniBandPartitionOnSite Failure", applicationErr.Error())
}

func TestDeleteInfiniBandPartitionSuite(t *testing.T) {
	suite.Run(t, new(DeleteInfiniBandPartitionTestSuite))
}
