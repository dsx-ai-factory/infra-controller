// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	temporalworkflow "go.temporal.io/sdk/workflow"
)

type InventoryExpectedSwitchTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *InventoryExpectedSwitchTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *InventoryExpectedSwitchTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *InventoryExpectedSwitchTestSuite) Test_DiscoverExpectedSwitchInventory_Success() {
	var inventoryManager iActivity.ManageExpectedSwitchInventory

	s.env.RegisterActivity(inventoryManager.DiscoverExpectedSwitchInventory)
	s.env.OnActivity(inventoryManager.DiscoverExpectedSwitchInventory, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DiscoverExpectedSwitchInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *InventoryExpectedSwitchTestSuite) Test_DiscoverExpectedSwitchInventory_ActivityFails() {
	var inventoryManager iActivity.ManageExpectedSwitchInventory

	errMsg := "Site Controller communication error"

	s.env.RegisterActivity(inventoryManager.DiscoverExpectedSwitchInventory)
	s.env.OnActivity(inventoryManager.DiscoverExpectedSwitchInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	s.env.ExecuteWorkflow(DiscoverExpectedSwitchInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestInventoryExpectedSwitchTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryExpectedSwitchTestSuite))
}

type CreateExpectedSwitchTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cests *CreateExpectedSwitchTestSuite) SetupTest() {
	cests.env = cests.NewTestWorkflowEnvironment()
}

func (cests *CreateExpectedSwitchTestSuite) AfterTest(suiteName, testName string) {
	cests.env.AssertExpectations(cests.T())
}

func (cests *CreateExpectedSwitchTestSuite) Test_CreateExpectedSwitch_Success() {
	var expectedSwitchManager iActivity.ManageExpectedSwitch

	request := &corev1.ExpectedSwitch{
		ExpectedSwitchId:   &corev1.UUID{Value: "test-create-workflow-001"},
		BmcMacAddress:      "00:11:22:33:44:55",
		SwitchSerialNumber: "SWITCH-001",
	}

	// Mock CreateExpectedSwitchOnSite activity
	cests.env.RegisterActivity(expectedSwitchManager.CreateExpectedSwitchOnSite)
	cests.env.OnActivity(expectedSwitchManager.CreateExpectedSwitchOnSite, mock.Anything, mock.Anything).Return(nil)

	cests.env.OnGetVersion(
		removeCreateExpectedSwitchOnFlowChangeID,
		temporalworkflow.DefaultVersion,
		removeCreateExpectedSwitchOnFlowVersion,
	).Return(removeCreateExpectedSwitchOnFlowVersion)
	cests.env.OnActivity(expectedSwitchManager.CreateExpectedSwitchOnFlow, mock.Anything, mock.Anything).Return(nil).Maybe()

	// Execute CreateExpectedSwitch workflow
	cests.env.ExecuteWorkflow(CreateExpectedSwitch, request)
	cests.True(cests.env.IsWorkflowCompleted())
	cests.NoError(cests.env.GetWorkflowError())
	cests.env.AssertActivityNumberOfCalls(cests.T(), "CreateExpectedSwitchOnFlow", 0)
}

func (cests *CreateExpectedSwitchTestSuite) Test_CreateExpectedSwitch_LegacyVersion_FlowFailure() {
	var expectedSwitchManager iActivity.ManageExpectedSwitch

	request := &corev1.ExpectedSwitch{
		ExpectedSwitchId:   &corev1.UUID{Value: "test-create-workflow-001"},
		BmcMacAddress:      "00:11:22:33:44:55",
		SwitchSerialNumber: "SWITCH-001",
	}

	cests.env.RegisterActivity(expectedSwitchManager.CreateExpectedSwitchOnSite)
	cests.env.OnActivity(expectedSwitchManager.CreateExpectedSwitchOnSite, mock.Anything, mock.Anything).Return(nil)

	cests.env.OnGetVersion(
		removeCreateExpectedSwitchOnFlowChangeID,
		temporalworkflow.DefaultVersion,
		removeCreateExpectedSwitchOnFlowVersion,
	).Return(temporalworkflow.DefaultVersion)

	cests.env.RegisterActivity(expectedSwitchManager.CreateExpectedSwitchOnFlow)
	cests.env.OnActivity(expectedSwitchManager.CreateExpectedSwitchOnFlow, mock.Anything, mock.Anything).Return(errors.New("Flow communication error"))

	cests.env.ExecuteWorkflow(CreateExpectedSwitch, request)
	cests.True(cests.env.IsWorkflowCompleted())
	cests.NoError(cests.env.GetWorkflowError())
}

func (cests *CreateExpectedSwitchTestSuite) Test_CreateExpectedSwitch_Failure() {
	var expectedSwitchManager iActivity.ManageExpectedSwitch

	request := &corev1.ExpectedSwitch{
		ExpectedSwitchId:   &corev1.UUID{Value: "test-create-workflow-001"},
		BmcMacAddress:      "00:11:22:33:44:55",
		SwitchSerialNumber: "SWITCH-001",
	}

	errMsg := "Site Controller communication error"

	// Mock CreateExpectedSwitchOnSite activity
	cests.env.RegisterActivity(expectedSwitchManager.CreateExpectedSwitchOnSite)
	cests.env.OnActivity(expectedSwitchManager.CreateExpectedSwitchOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute CreateExpectedSwitch workflow
	cests.env.ExecuteWorkflow(CreateExpectedSwitch, request)
	cests.True(cests.env.IsWorkflowCompleted())
	cests.Error(cests.env.GetWorkflowError())
}

func TestCreateExpectedSwitchTestSuite(t *testing.T) {
	suite.Run(t, new(CreateExpectedSwitchTestSuite))
}

type UpdateExpectedSwitchTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (uests *UpdateExpectedSwitchTestSuite) SetupTest() {
	uests.env = uests.NewTestWorkflowEnvironment()
}

func (uests *UpdateExpectedSwitchTestSuite) AfterTest(suiteName, testName string) {
	uests.env.AssertExpectations(uests.T())
}

func (uests *UpdateExpectedSwitchTestSuite) Test_UpdateExpectedSwitch_Success() {
	var expectedSwitchManager iActivity.ManageExpectedSwitch

	request := &corev1.ExpectedSwitch{
		ExpectedSwitchId:   &corev1.UUID{Value: "test-update-workflow-001"},
		BmcMacAddress:      "00:11:22:33:44:55",
		SwitchSerialNumber: "SWITCH-001",
	}

	// Mock UpdateExpectedSwitchOnSite activity
	uests.env.RegisterActivity(expectedSwitchManager.UpdateExpectedSwitchOnSite)
	uests.env.OnActivity(expectedSwitchManager.UpdateExpectedSwitchOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute workflow
	uests.env.ExecuteWorkflow(UpdateExpectedSwitch, request)
	uests.True(uests.env.IsWorkflowCompleted())
	uests.NoError(uests.env.GetWorkflowError())
}

func (uests *UpdateExpectedSwitchTestSuite) Test_UpdateExpectedSwitch_Failure() {
	var expectedSwitchManager iActivity.ManageExpectedSwitch

	request := &corev1.ExpectedSwitch{
		ExpectedSwitchId:   &corev1.UUID{Value: "test-update-workflow-001"},
		BmcMacAddress:      "00:11:22:33:44:55",
		SwitchSerialNumber: "SWITCH-001",
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateExpectedSwitchOnSite activity
	uests.env.RegisterActivity(expectedSwitchManager.UpdateExpectedSwitchOnSite)
	uests.env.OnActivity(expectedSwitchManager.UpdateExpectedSwitchOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateExpectedSwitch workflow
	uests.env.ExecuteWorkflow(UpdateExpectedSwitch, request)
	uests.True(uests.env.IsWorkflowCompleted())
	uests.Error(uests.env.GetWorkflowError())
}

func TestUpdateExpectedSwitchTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateExpectedSwitchTestSuite))
}

type DeleteExpectedSwitchTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (dests *DeleteExpectedSwitchTestSuite) SetupTest() {
	dests.env = dests.NewTestWorkflowEnvironment()
}

func (dests *DeleteExpectedSwitchTestSuite) AfterTest(suiteName, testName string) {
	dests.env.AssertExpectations(dests.T())
}

func (dests *DeleteExpectedSwitchTestSuite) Test_DeleteExpectedSwitch_Success() {
	var expectedSwitchManager iActivity.ManageExpectedSwitch

	request := &corev1.ExpectedSwitchRequest{
		ExpectedSwitchId: &corev1.UUID{Value: "test-delete-workflow-001"},
		BmcMacAddress:    "00:11:22:33:44:55",
	}

	// Mock DeleteExpectedSwitchOnSite activity
	dests.env.RegisterActivity(expectedSwitchManager.DeleteExpectedSwitchOnSite)
	dests.env.OnActivity(expectedSwitchManager.DeleteExpectedSwitchOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	dests.env.ExecuteWorkflow(DeleteExpectedSwitch, request)
	dests.True(dests.env.IsWorkflowCompleted())
	dests.NoError(dests.env.GetWorkflowError())
}

func (dests *DeleteExpectedSwitchTestSuite) Test_DeleteExpectedSwitch_Failure() {
	var expectedSwitchManager iActivity.ManageExpectedSwitch

	request := &corev1.ExpectedSwitchRequest{
		ExpectedSwitchId: &corev1.UUID{Value: "test-delete-workflow-001"},
		BmcMacAddress:    "00:11:22:33:44:55",
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteExpectedSwitchOnSite activity
	dests.env.RegisterActivity(expectedSwitchManager.DeleteExpectedSwitchOnSite)
	dests.env.OnActivity(expectedSwitchManager.DeleteExpectedSwitchOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteExpectedSwitch workflow
	dests.env.ExecuteWorkflow(DeleteExpectedSwitch, request)
	dests.True(dests.env.IsWorkflowCompleted())
	dests.Error(dests.env.GetWorkflowError())
}

func TestDeleteExpectedSwitchTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteExpectedSwitchTestSuite))
}
