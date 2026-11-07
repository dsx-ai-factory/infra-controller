// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshkeygroup

import (
	"context"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	tmocks "go.temporal.io/sdk/mocks"

	sshKeyGroupActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/sshkeygroup"
)

type DeleteSSHKeyGroupTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *DeleteSSHKeyGroupTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *DeleteSSHKeyGroupTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *DeleteSSHKeyGroupTestSuite) Test_DeleteSSHKeyGroupWorkflow_Success() {
	var sshKeyGroupManager sshKeyGroupActivity.ManageSSHKeyGroup

	siteID := uuid.New()
	sshKeyGroupID := uuid.New()

	// Mock DeleteSSHKeyGroupViaSiteAgent activity
	s.env.RegisterActivity(sshKeyGroupManager.DeleteSSHKeyGroupViaSiteAgent)
	s.env.OnActivity(sshKeyGroupManager.DeleteSSHKeyGroupViaSiteAgent, mock.Anything, siteID, sshKeyGroupID).Return(nil)

	// execute DeleteSSHKeyGroup workflow
	s.env.ExecuteWorkflow(DeleteSSHKeyGroup, siteID, sshKeyGroupID)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *DeleteSSHKeyGroupTestSuite) Test_DeleteSSHKeyGroupWorkflow_ActivityFails() {
	var sshKeyGroupManager sshKeyGroupActivity.ManageSSHKeyGroup

	siteID := uuid.New()
	sshKeyGroupID := uuid.New()

	// Mock DeleteSSHKeyGroupViaSiteAgent activity failure
	s.env.RegisterActivity(sshKeyGroupManager.DeleteSSHKeyGroupViaSiteAgent)
	s.env.OnActivity(sshKeyGroupManager.DeleteSSHKeyGroupViaSiteAgent, mock.Anything, siteID, sshKeyGroupID).Return(errors.New("DeleteSSHKeyGroupViaSiteAgent Failure"))

	// execute createVPC workflow
	s.env.ExecuteWorkflow(DeleteSSHKeyGroup, siteID, sshKeyGroupID)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("DeleteSSHKeyGroupViaSiteAgent Failure", applicationErr.Error())
}

func (s *DeleteSSHKeyGroupTestSuite) Test_ExecuteDeleteSSHKeyGroupWorkflow_Success() {
	ctx := context.Background()

	siteID := uuid.New()
	sshKeyGroupID := uuid.New()

	wid := "test-workflow-id"

	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	tc := &tmocks.Client{}

	tc.Mock.On("ExecuteWorkflow", context.Background(), mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.Anything, siteID, sshKeyGroupID).Return(wrun, nil)

	rwid, err := ExecuteDeleteSSHKeyGroupWorkflow(ctx, tc, siteID, sshKeyGroupID)
	s.NoError(err)
	s.Equal(wid, *rwid)
}

func TestDeleteSSHKeyGroupSuite(t *testing.T) {
	suite.Run(t, new(DeleteSSHKeyGroupTestSuite))
}
