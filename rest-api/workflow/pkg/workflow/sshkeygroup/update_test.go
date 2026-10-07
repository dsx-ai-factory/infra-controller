// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshkeygroup

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	sshKeyGroupActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/sshkeygroup"
)

type UpdateSSHKeyGroupTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateSSHKeyGroupTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateSSHKeyGroupTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateSSHKeyGroupTestSuite) Test_UpdateSSHKeyGroupInventory_Success() {
	var sshKeyGroupManager sshKeyGroupActivity.ManageSSHKeyGroup

	siteID := uuid.New()
	sshKeyGroupIDs := []string{uuid.New().String(), uuid.New().String()}

	sshKeyGroupInventory := &corev1.SSHKeyGroupInventory{
		TenantKeysets: []*corev1.TenantKeyset{
			{
				KeysetIdentifier: &corev1.TenantKeysetIdentifier{
					KeysetId: "1234",
				},
				Version: "1234",
			},
			{
				KeysetIdentifier: &corev1.TenantKeysetIdentifier{
					KeysetId: "1235",
				},
				Version: "1235",
			},
		},
	}

	// Mock UpdateSSHKeyGroupsInDB activity
	s.env.RegisterActivity(sshKeyGroupManager.UpdateSSHKeyGroupsInDB)
	s.env.OnActivity(sshKeyGroupManager.UpdateSSHKeyGroupsInDB, mock.Anything, mock.Anything, mock.Anything).Return(sshKeyGroupIDs, nil)
	s.env.OnActivity(sshKeyGroupManager.UpdateSSHKeyGroupStatusInDB, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateSSHKeyGroupInventory workflow
	s.env.ExecuteWorkflow(UpdateSSHKeyGroupInventory, siteID.String(), sshKeyGroupInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateSSHKeyGroupTestSuite) Test_UpdateSSHKeyGroupInventory_ActivityFails() {
	var sshKeyGroupManager sshKeyGroupActivity.ManageSSHKeyGroup

	siteID := uuid.New()
	sshKeyGroupInventory := &corev1.SSHKeyGroupInventory{
		TenantKeysets: []*corev1.TenantKeyset{
			{
				KeysetIdentifier: &corev1.TenantKeysetIdentifier{
					KeysetId: "1234",
				},
				Version: "1234",
			},
			{
				KeysetIdentifier: &corev1.TenantKeysetIdentifier{
					KeysetId: "1235",
				},
				Version: "1235",
			},
		},
	}

	// Mock UpdateVpcsViaSiteAgent activity failure
	s.env.RegisterActivity(sshKeyGroupManager.UpdateSSHKeyGroupsInDB)
	s.env.OnActivity(sshKeyGroupManager.UpdateSSHKeyGroupsInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil, errors.New("UpdateSSHKeyGroupInventory Failure"))

	// execute UpdateVPCStatus workflow
	s.env.ExecuteWorkflow(UpdateSSHKeyGroupInventory, siteID.String(), sshKeyGroupInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateSSHKeyGroupInventory Failure", applicationErr.Error())
}

func TestUpdateSSHKeyGroupSuite(t *testing.T) {
	suite.Run(t, new(UpdateSSHKeyGroupTestSuite))
}
