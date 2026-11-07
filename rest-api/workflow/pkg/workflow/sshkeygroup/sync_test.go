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

	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	sshKeyGroupActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/sshkeygroup"
)

type SyncSSHKeyGroupTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *SyncSSHKeyGroupTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *SyncSSHKeyGroupTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *SyncSSHKeyGroupTestSuite) Test_SyncSSHKeyGroupWorkflow_Success() {
	var sshKeyGroupManager sshKeyGroupActivity.ManageSSHKeyGroup

	siteID := uuid.New()
	sshKeyGroupID := uuid.New()

	// Mock SyncSSHKeyGroupViaSiteAgent activity
	s.env.RegisterActivity(sshKeyGroupManager.SyncSSHKeyGroupViaSiteAgent)
	s.env.OnActivity(sshKeyGroupManager.SyncSSHKeyGroupViaSiteAgent, mock.Anything, siteID, sshKeyGroupID, mock.Anything).Return(nil)

	// Mock UpdateSSHKeyGroupStatusInDB activity
	s.env.RegisterActivity(sshKeyGroupManager.UpdateSSHKeyGroupStatusInDB)
	s.env.OnActivity(sshKeyGroupManager.UpdateSSHKeyGroupStatusInDB, mock.Anything, sshKeyGroupID.String()).Return(nil)

	// Execute SyncSSHKeyGroup workflow
	s.env.ExecuteWorkflow(SyncSSHKeyGroup, siteID, sshKeyGroupID, mock.Anything)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *SyncSSHKeyGroupTestSuite) Test_SyncSSHKeyGroupWorkflow_Success_With_UpdateSSHKeyGroupStatusInDB_Failure() {
	var sshKeyGroupManager sshKeyGroupActivity.ManageSSHKeyGroup

	siteID := uuid.New()
	sshKeyGroupID := uuid.New()

	// Mock SyncSSHKeyGroupViaSiteAgent activity
	s.env.RegisterActivity(sshKeyGroupManager.SyncSSHKeyGroupViaSiteAgent)
	s.env.OnActivity(sshKeyGroupManager.SyncSSHKeyGroupViaSiteAgent, mock.Anything, siteID, sshKeyGroupID, mock.Anything).Return(nil)

	// Mock UpdateSSHKeyGroupStatusInDB activity
	s.env.RegisterActivity(sshKeyGroupManager.UpdateSSHKeyGroupStatusInDB)
	s.env.OnActivity(sshKeyGroupManager.UpdateSSHKeyGroupStatusInDB, mock.Anything, sshKeyGroupID.String()).Return(errors.New("UpdateSSHKeyGroupStatusInDB Failure"))

	// Execute SyncSSHKeyGroup workflow
	s.env.ExecuteWorkflow(SyncSSHKeyGroup, siteID, sshKeyGroupID, mock.Anything)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *SyncSSHKeyGroupTestSuite) Test_SyncSSHKeyGroupWorkflow_SyncSSHKeyGroupViaSiteAgent_Failure() {

	var sshKeyGroupManager sshKeyGroupActivity.ManageSSHKeyGroup

	siteID := uuid.New()
	sshKeyGroupID := uuid.New()

	// Mock SyncSSHKeyGroupViaSiteAgent activity failure
	s.env.RegisterActivity(sshKeyGroupManager.SyncSSHKeyGroupViaSiteAgent)
	s.env.OnActivity(sshKeyGroupManager.SyncSSHKeyGroupViaSiteAgent, mock.Anything, siteID, sshKeyGroupID, mock.Anything).Return(errors.New("SyncSSHKeyGroupViaSiteAgent Failure"))

	// Mock UpdateSSHKeyGroupStatusInDB activity
	s.env.RegisterActivity(sshKeyGroupManager.UpdateSSHKeyGroupStatusInDB)

	// Execute SyncSSHKeyGroup workflow
	s.env.ExecuteWorkflow(SyncSSHKeyGroup, siteID, sshKeyGroupID, mock.Anything)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("SyncSSHKeyGroupViaSiteAgent Failure", applicationErr.Error())
}

func (s *SyncSSHKeyGroupTestSuite) Test_ExecuteSyncSSHKeyGroupWorkflow_Success() {
	ctx := context.Background()
	siteID := uuid.New()
	sshKeyGroupID := uuid.New()

	wid := "test-workflow-id"

	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	tc := &tmocks.Client{}

	tc.Mock.On("ExecuteWorkflow", context.Background(), mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.Anything, siteID, sshKeyGroupID, mock.Anything).Return(wrun, nil)

	rwid, err := ExecuteSyncSSHKeyGroupWorkflow(ctx, tc, siteID, sshKeyGroupID, mock.Anything)
	s.NoError(err)
	s.Equal(wid, *rwid)
}

func TestSyncSSHKeyGroupGroupSuite(t *testing.T) {
	suite.Run(t, new(SyncSSHKeyGroupTestSuite))
}
