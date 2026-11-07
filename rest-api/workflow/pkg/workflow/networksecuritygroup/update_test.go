// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package networksecuritygroup

import (
	"errors"
	"testing"

	networkSecurityGroupActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/networksecuritygroup"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type UpdateNetworkSecurityGroupTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateNetworkSecurityGroupTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateNetworkSecurityGroupTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateNetworkSecurityGroupTestSuite) Test_UpdateNetworkSecurityGroupInventory_Success() {

	var networkSecurityGroupManager networkSecurityGroupActivity.ManageNetworkSecurityGroup

	siteID := uuid.New()
	networkSecurityGroupInventory := &corev1.NetworkSecurityGroupInventory{
		NetworkSecurityGroups: []*corev1.NetworkSecurityGroup{
			{
				Id: uuid.NewString(),
			},
			{
				Id: uuid.NewString(),
			},
		},
	}

	// Mock UpdateNetworkSecurityGroupViaSiteAgent activity
	s.env.RegisterActivity(networkSecurityGroupManager.UpdateNetworkSecurityGroupsInDB)
	s.env.OnActivity(networkSecurityGroupManager.UpdateNetworkSecurityGroupsInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateNetworkSecurityGroupInventory workflow
	s.env.ExecuteWorkflow(UpdateNetworkSecurityGroupInventory, siteID.String(), networkSecurityGroupInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateNetworkSecurityGroupTestSuite) Test_UpdateNetworkSecurityGroupInventory_ActivityFails() {

	var networkSecurityGroupManager networkSecurityGroupActivity.ManageNetworkSecurityGroup

	siteID := uuid.New()
	networkSecurityGroupInventory := &corev1.NetworkSecurityGroupInventory{
		NetworkSecurityGroups: []*corev1.NetworkSecurityGroup{
			{
				Id: uuid.NewString(),
			},
			{
				Id: uuid.NewString(),
			},
		},
	}

	// Mock UpdateNetworkSecurityGroupsViaSiteAgent activity failure
	s.env.RegisterActivity(networkSecurityGroupManager.UpdateNetworkSecurityGroupsInDB)
	s.env.OnActivity(networkSecurityGroupManager.UpdateNetworkSecurityGroupsInDB, mock.Anything, mock.Anything, mock.Anything).Return(errors.New("UpdateNetworkSecurityGroupInventory Failure"))

	// execute UpdateNetworkSecurityGroupStatus workflow
	s.env.ExecuteWorkflow(UpdateNetworkSecurityGroupInventory, siteID.String(), networkSecurityGroupInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateNetworkSecurityGroupInventory Failure", applicationErr.Error())
}

func TestUpdateNetworkSecurityGroupSuite(t *testing.T) {
	suite.Run(t, new(UpdateNetworkSecurityGroupTestSuite))
}
