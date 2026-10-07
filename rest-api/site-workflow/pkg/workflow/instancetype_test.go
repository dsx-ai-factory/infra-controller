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

func (s *UpdateInstanceTypeTestSuite) Test_UpdateInstanceType_Success() {
	var machineManager iActivity.ManageInstanceType

	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.UpdateInstanceTypeRequest{
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

	// Mock UpdateInstanceTypeOnSiteActivity activity
	s.env.RegisterActivity(machineManager.UpdateInstanceTypeOnSite)
	s.env.OnActivity(machineManager.UpdateInstanceTypeOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateMachineInventory workflow
	s.env.ExecuteWorkflow(UpdateInstanceType, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateInstanceTypeTestSuite) Test_UpdateInstanceType_Failure() {
	var machineManager iActivity.ManageInstanceType

	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.UpdateInstanceTypeRequest{
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

	// Mock UpdateInstanceTypeOnSiteActivity activity
	s.env.RegisterActivity(machineManager.UpdateInstanceTypeOnSite)
	s.env.OnActivity(machineManager.UpdateInstanceTypeOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateMachineInventory workflow
	s.env.ExecuteWorkflow(UpdateInstanceType, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestUpdateInstanceTypeTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateInstanceTypeTestSuite))
}

type CreateInstanceTypeTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *CreateInstanceTypeTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *CreateInstanceTypeTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *CreateInstanceTypeTestSuite) Test_CreateInstanceType_Success() {
	var machineManager iActivity.ManageInstanceType

	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.CreateInstanceTypeRequest{
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

	// Mock CreateInstanceTypeOnSiteActivity activity
	s.env.RegisterActivity(machineManager.CreateInstanceTypeOnSite)
	s.env.OnActivity(machineManager.CreateInstanceTypeOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(CreateInstanceType, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *CreateInstanceTypeTestSuite) Test_CreateInstanceType_Failure() {
	var machineManager iActivity.ManageInstanceType

	labelKey := "key1"
	labelValue := "value1"

	request := &corev1.CreateInstanceTypeRequest{
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

	// Mock CreateInstanceTypeOnSiteActivity activity
	s.env.RegisterActivity(machineManager.CreateInstanceTypeOnSite)
	s.env.OnActivity(machineManager.CreateInstanceTypeOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute CreateMachineInventory workflow
	s.env.ExecuteWorkflow(CreateInstanceType, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestCreateInstanceTypeTestSuite(t *testing.T) {
	suite.Run(t, new(CreateInstanceTypeTestSuite))
}

type DeleteInstanceTypeTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *DeleteInstanceTypeTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *DeleteInstanceTypeTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *DeleteInstanceTypeTestSuite) Test_DeleteInstanceType_Success() {
	var instanceTypeManager iActivity.ManageInstanceType

	request := &corev1.DeleteInstanceTypeRequest{
		Id: uuid.NewString(),
	}

	// Mock DeleteInstanceTypeOnSiteActivity activity
	s.env.RegisterActivity(instanceTypeManager.DeleteInstanceTypeOnSite)
	s.env.OnActivity(instanceTypeManager.DeleteInstanceTypeOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DeleteInstanceType, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *DeleteInstanceTypeTestSuite) Test_DeleteInstanceType_Failure() {
	var machineManager iActivity.ManageInstanceType

	request := &corev1.DeleteInstanceTypeRequest{
		Id: uuid.NewString(),
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteInstanceTypeOnSiteActivity activity
	s.env.RegisterActivity(machineManager.DeleteInstanceTypeOnSite)
	s.env.OnActivity(machineManager.DeleteInstanceTypeOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteMachineInventory workflow
	s.env.ExecuteWorkflow(DeleteInstanceType, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestDeleteInstanceTypeTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteInstanceTypeTestSuite))
}

type AssociateMachinesWithInstanceTypeTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *AssociateMachinesWithInstanceTypeTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *AssociateMachinesWithInstanceTypeTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *AssociateMachinesWithInstanceTypeTestSuite) Test_AssociateMachinesWithInstanceType_Success() {
	var instanceTypeManager iActivity.ManageInstanceType

	request := &corev1.AssociateMachinesWithInstanceTypeRequest{
		InstanceTypeId: uuid.NewString(),
		MachineIds:     []string{uuid.NewString()},
	}

	// Mock DeleteInstanceTypeOnSiteActivity activity
	s.env.RegisterActivity(instanceTypeManager.AssociateMachinesWithInstanceTypeOnSite)
	s.env.OnActivity(instanceTypeManager.AssociateMachinesWithInstanceTypeOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(AssociateMachinesWithInstanceType, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *AssociateMachinesWithInstanceTypeTestSuite) Test_AssociateMachinesWithInstanceType_Failure() {
	var machineManager iActivity.ManageInstanceType

	request := &corev1.AssociateMachinesWithInstanceTypeRequest{
		InstanceTypeId: uuid.NewString(),
		MachineIds:     []string{uuid.NewString()},
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteInstanceTypeOnSiteActivity activity
	s.env.RegisterActivity(machineManager.AssociateMachinesWithInstanceTypeOnSite)
	s.env.OnActivity(machineManager.AssociateMachinesWithInstanceTypeOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteMachineInventory workflow
	s.env.ExecuteWorkflow(AssociateMachinesWithInstanceType, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestAssociateMachinesWithInstanceTypeTestSuite(t *testing.T) {
	suite.Run(t, new(AssociateMachinesWithInstanceTypeTestSuite))
}

type RemoveMachineInstanceTypeAssociationTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *RemoveMachineInstanceTypeAssociationTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *RemoveMachineInstanceTypeAssociationTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *RemoveMachineInstanceTypeAssociationTestSuite) Test_RemoveMachineInstanceTypeAssociation_Success() {
	var instanceTypeManager iActivity.ManageInstanceType

	request := &corev1.RemoveMachineInstanceTypeAssociationRequest{
		MachineId: uuid.NewString(),
	}

	// Mock DeleteInstanceTypeOnSiteActivity activity
	s.env.RegisterActivity(instanceTypeManager.RemoveMachineInstanceTypeAssociationOnSite)
	s.env.OnActivity(instanceTypeManager.RemoveMachineInstanceTypeAssociationOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(RemoveMachineInstanceTypeAssociation, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *RemoveMachineInstanceTypeAssociationTestSuite) Test_RemoveMachineInstanceTypeAssociation_Failure() {
	var machineManager iActivity.ManageInstanceType

	request := &corev1.RemoveMachineInstanceTypeAssociationRequest{
		MachineId: uuid.NewString(),
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteInstanceTypeOnSiteActivity activity
	s.env.RegisterActivity(machineManager.RemoveMachineInstanceTypeAssociationOnSite)
	s.env.OnActivity(machineManager.RemoveMachineInstanceTypeAssociationOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteMachineInventory workflow
	s.env.ExecuteWorkflow(RemoveMachineInstanceTypeAssociation, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestRemoveMachineInstanceTypeAssociationTestSuite(t *testing.T) {
	suite.Run(t, new(RemoveMachineInstanceTypeAssociationTestSuite))
}

type InventoryInstanceTypeTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *InventoryInstanceTypeTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *InventoryInstanceTypeTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *InventoryInstanceTypeTestSuite) Test_DiscoverInstanceTypeInventory_Success() {
	var inventoryManager iActivity.ManageInstanceTypeInventory

	s.env.RegisterActivity(inventoryManager.DiscoverInstanceTypeInventory)
	s.env.OnActivity(inventoryManager.DiscoverInstanceTypeInventory, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DiscoverInstanceTypeInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *InventoryInstanceTypeTestSuite) Test_DiscoverInstanceTypeInventory_ActivityFails() {
	var inventoryManager iActivity.ManageInstanceTypeInventory

	errMsg := "Site Controller communication error"

	s.env.RegisterActivity(inventoryManager.DiscoverInstanceTypeInventory)
	s.env.OnActivity(inventoryManager.DiscoverInstanceTypeInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	s.env.ExecuteWorkflow(DiscoverInstanceTypeInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestInventoryInstanceTypeTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryInstanceTypeTestSuite))
}
