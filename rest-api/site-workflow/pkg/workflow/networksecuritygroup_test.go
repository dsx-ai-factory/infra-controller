// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"

	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
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

func (s *UpdateNetworkSecurityGroupTestSuite) Test_UpdateNetworkSecurityGroup_Success() {
	var machineManager iActivity.ManageNetworkSecurityGroup

	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.UpdateNetworkSecurityGroupRequest{
		Id: uuid.NewString(),
		Metadata: &corev1.Metadata{
			Name:        "updated_name",
			Description: "updated_description",
			Labels: []*corev1.Label{
				{
					Key:   labelKey,
					Value: &labelValue,
				},
			},
		},
	}

	// Mock UpdateNetworkSecurityGroupOnSiteActivity activity
	s.env.RegisterActivity(machineManager.UpdateNetworkSecurityGroupOnSite)
	s.env.OnActivity(machineManager.UpdateNetworkSecurityGroupOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateMachineInventory workflow
	s.env.ExecuteWorkflow(UpdateNetworkSecurityGroup, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateNetworkSecurityGroupTestSuite) Test_UpdateNetworkSecurityGroup_Failure() {
	var machineManager iActivity.ManageNetworkSecurityGroup

	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.UpdateNetworkSecurityGroupRequest{
		Id: uuid.NewString(),
		Metadata: &corev1.Metadata{
			Name:        "updated_name",
			Description: "updated_description",
			Labels: []*corev1.Label{
				{
					Key:   labelKey,
					Value: &labelValue,
				},
			},
		},
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateNetworkSecurityGroupOnSiteActivity activity
	s.env.RegisterActivity(machineManager.UpdateNetworkSecurityGroupOnSite)
	s.env.OnActivity(machineManager.UpdateNetworkSecurityGroupOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateMachineInventory workflow
	s.env.ExecuteWorkflow(UpdateNetworkSecurityGroup, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestUpdateNetworkSecurityGroupTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateNetworkSecurityGroupTestSuite))
}

type CreateNetworkSecurityGroupTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *CreateNetworkSecurityGroupTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *CreateNetworkSecurityGroupTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *CreateNetworkSecurityGroupTestSuite) Test_CreateNetworkSecurityGroup_Success() {
	var machineManager iActivity.ManageNetworkSecurityGroup

	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.CreateNetworkSecurityGroupRequest{
		Id: util.GetStrPtr(uuid.NewString()),
		Metadata: &corev1.Metadata{
			Name:        "updated_name",
			Description: "updated_description",
			Labels: []*corev1.Label{
				{
					Key:   labelKey,
					Value: &labelValue,
				},
			},
		},
	}

	// Mock CreateNetworkSecurityGroupOnSiteActivity activity
	s.env.RegisterActivity(machineManager.CreateNetworkSecurityGroupOnSite)
	s.env.OnActivity(machineManager.CreateNetworkSecurityGroupOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(CreateNetworkSecurityGroup, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *CreateNetworkSecurityGroupTestSuite) Test_CreateNetworkSecurityGroup_Failure() {
	var machineManager iActivity.ManageNetworkSecurityGroup

	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.CreateNetworkSecurityGroupRequest{
		Id: util.GetStrPtr(uuid.NewString()),
		Metadata: &corev1.Metadata{
			Name:        "updated_name",
			Description: "updated_description",
			Labels: []*corev1.Label{
				{
					Key:   labelKey,
					Value: &labelValue,
				},
			},
		},
	}

	errMsg := "Site Controller communication error"

	// Mock CreateNetworkSecurityGroupOnSiteActivity activity
	s.env.RegisterActivity(machineManager.CreateNetworkSecurityGroupOnSite)
	s.env.OnActivity(machineManager.CreateNetworkSecurityGroupOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute CreateMachineInventory workflow
	s.env.ExecuteWorkflow(CreateNetworkSecurityGroup, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestCreateNetworkSecurityGroupTestSuite(t *testing.T) {
	suite.Run(t, new(CreateNetworkSecurityGroupTestSuite))
}

type DeleteNetworkSecurityGroupTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *DeleteNetworkSecurityGroupTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *DeleteNetworkSecurityGroupTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *DeleteNetworkSecurityGroupTestSuite) Test_DeleteNetworkSecurityGroup_Success() {
	var networkSecurityGroupManager iActivity.ManageNetworkSecurityGroup

	request := &corev1.DeleteNetworkSecurityGroupRequest{
		Id: uuid.NewString(),
	}

	// Mock DeleteNetworkSecurityGroupOnSiteActivity activity
	s.env.RegisterActivity(networkSecurityGroupManager.DeleteNetworkSecurityGroupOnSite)
	s.env.OnActivity(networkSecurityGroupManager.DeleteNetworkSecurityGroupOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DeleteNetworkSecurityGroup, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *DeleteNetworkSecurityGroupTestSuite) Test_DeleteNetworkSecurityGroup_Failure() {
	var machineManager iActivity.ManageNetworkSecurityGroup

	request := &corev1.DeleteNetworkSecurityGroupRequest{
		Id: uuid.NewString(),
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteNetworkSecurityGroupOnSiteActivity activity
	s.env.RegisterActivity(machineManager.DeleteNetworkSecurityGroupOnSite)
	s.env.OnActivity(machineManager.DeleteNetworkSecurityGroupOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteMachineInventory workflow
	s.env.ExecuteWorkflow(DeleteNetworkSecurityGroup, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestDeleteNetworkSecurityGroupTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteNetworkSecurityGroupTestSuite))
}

type InventoryNetworkSecurityGroupTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *InventoryNetworkSecurityGroupTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *InventoryNetworkSecurityGroupTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *InventoryNetworkSecurityGroupTestSuite) Test_DiscoverNetworkSecurityGroupInventory_Success() {
	var inventoryManager iActivity.ManageNetworkSecurityGroupInventory

	s.env.RegisterActivity(inventoryManager.DiscoverNetworkSecurityGroupInventory)
	s.env.OnActivity(inventoryManager.DiscoverNetworkSecurityGroupInventory, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DiscoverNetworkSecurityGroupInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *InventoryNetworkSecurityGroupTestSuite) Test_DiscoverNetworkSecurityGroupInventory_ActivityFails() {
	var inventoryManager iActivity.ManageNetworkSecurityGroupInventory

	errMsg := "Site Controller communication error"

	s.env.RegisterActivity(inventoryManager.DiscoverNetworkSecurityGroupInventory)
	s.env.OnActivity(inventoryManager.DiscoverNetworkSecurityGroupInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	s.env.ExecuteWorkflow(DiscoverNetworkSecurityGroupInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestInventoryNetworkSecurityGroupTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryNetworkSecurityGroupTestSuite))
}
