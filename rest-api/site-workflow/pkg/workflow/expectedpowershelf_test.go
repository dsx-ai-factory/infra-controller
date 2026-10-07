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

type InventoryExpectedPowerShelfTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *InventoryExpectedPowerShelfTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *InventoryExpectedPowerShelfTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *InventoryExpectedPowerShelfTestSuite) Test_DiscoverExpectedPowerShelfInventory_Success() {
	var inventoryManager iActivity.ManageExpectedPowerShelfInventory

	s.env.RegisterActivity(inventoryManager.DiscoverExpectedPowerShelfInventory)
	s.env.OnActivity(inventoryManager.DiscoverExpectedPowerShelfInventory, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DiscoverExpectedPowerShelfInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *InventoryExpectedPowerShelfTestSuite) Test_DiscoverExpectedPowerShelfInventory_ActivityFails() {
	var inventoryManager iActivity.ManageExpectedPowerShelfInventory

	errMsg := "Site Controller communication error"

	s.env.RegisterActivity(inventoryManager.DiscoverExpectedPowerShelfInventory)
	s.env.OnActivity(inventoryManager.DiscoverExpectedPowerShelfInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	s.env.ExecuteWorkflow(DiscoverExpectedPowerShelfInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestInventoryExpectedPowerShelfTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryExpectedPowerShelfTestSuite))
}

type CreateExpectedPowerShelfTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cepsts *CreateExpectedPowerShelfTestSuite) SetupTest() {
	cepsts.env = cepsts.NewTestWorkflowEnvironment()
}

func (cepsts *CreateExpectedPowerShelfTestSuite) AfterTest(suiteName, testName string) {
	cepsts.env.AssertExpectations(cepsts.T())
}

func (cepsts *CreateExpectedPowerShelfTestSuite) Test_CreateExpectedPowerShelf_Success() {
	var expectedPowerShelfManager iActivity.ManageExpectedPowerShelf

	request := &corev1.ExpectedPowerShelf{
		ExpectedPowerShelfId: &corev1.UUID{Value: "test-create-workflow-001"},
		BmcMacAddress:        "00:11:22:33:44:55",
		ShelfSerialNumber:    "SHELF-001",
	}

	// Mock CreateExpectedPowerShelfOnSite activity
	cepsts.env.RegisterActivity(expectedPowerShelfManager.CreateExpectedPowerShelfOnSite)
	cepsts.env.OnActivity(expectedPowerShelfManager.CreateExpectedPowerShelfOnSite, mock.Anything, mock.Anything).Return(nil)

	cepsts.env.OnGetVersion(
		removeCreateExpectedPowerShelfOnFlowChangeID,
		temporalworkflow.DefaultVersion,
		removeCreateExpectedPowerShelfOnFlowVersion,
	).Return(removeCreateExpectedPowerShelfOnFlowVersion)
	cepsts.env.OnActivity(expectedPowerShelfManager.CreateExpectedPowerShelfOnFlow, mock.Anything, mock.Anything).Return(nil).Maybe()

	// Execute CreateExpectedPowerShelf workflow
	cepsts.env.ExecuteWorkflow(CreateExpectedPowerShelf, request)
	cepsts.True(cepsts.env.IsWorkflowCompleted())
	cepsts.NoError(cepsts.env.GetWorkflowError())
	cepsts.env.AssertActivityNumberOfCalls(cepsts.T(), "CreateExpectedPowerShelfOnFlow", 0)
}

func (cepsts *CreateExpectedPowerShelfTestSuite) Test_CreateExpectedPowerShelf_LegacyVersion_FlowFailure() {
	var expectedPowerShelfManager iActivity.ManageExpectedPowerShelf

	request := &corev1.ExpectedPowerShelf{
		ExpectedPowerShelfId: &corev1.UUID{Value: "test-create-workflow-001"},
		BmcMacAddress:        "00:11:22:33:44:55",
		ShelfSerialNumber:    "SHELF-001",
	}

	cepsts.env.RegisterActivity(expectedPowerShelfManager.CreateExpectedPowerShelfOnSite)
	cepsts.env.OnActivity(expectedPowerShelfManager.CreateExpectedPowerShelfOnSite, mock.Anything, mock.Anything).Return(nil)

	cepsts.env.OnGetVersion(
		removeCreateExpectedPowerShelfOnFlowChangeID,
		temporalworkflow.DefaultVersion,
		removeCreateExpectedPowerShelfOnFlowVersion,
	).Return(temporalworkflow.DefaultVersion)

	cepsts.env.RegisterActivity(expectedPowerShelfManager.CreateExpectedPowerShelfOnFlow)
	cepsts.env.OnActivity(expectedPowerShelfManager.CreateExpectedPowerShelfOnFlow, mock.Anything, mock.Anything).Return(errors.New("Flow communication error"))

	cepsts.env.ExecuteWorkflow(CreateExpectedPowerShelf, request)
	cepsts.True(cepsts.env.IsWorkflowCompleted())
	cepsts.NoError(cepsts.env.GetWorkflowError())
}

func (cepsts *CreateExpectedPowerShelfTestSuite) Test_CreateExpectedPowerShelf_Failure() {
	var expectedPowerShelfManager iActivity.ManageExpectedPowerShelf

	request := &corev1.ExpectedPowerShelf{
		ExpectedPowerShelfId: &corev1.UUID{Value: "test-create-workflow-001"},
		BmcMacAddress:        "00:11:22:33:44:55",
		ShelfSerialNumber:    "SHELF-001",
	}

	errMsg := "Site Controller communication error"

	// Mock CreateExpectedPowerShelfOnSite activity
	cepsts.env.RegisterActivity(expectedPowerShelfManager.CreateExpectedPowerShelfOnSite)
	cepsts.env.OnActivity(expectedPowerShelfManager.CreateExpectedPowerShelfOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute CreateExpectedPowerShelf workflow
	cepsts.env.ExecuteWorkflow(CreateExpectedPowerShelf, request)
	cepsts.True(cepsts.env.IsWorkflowCompleted())
	cepsts.Error(cepsts.env.GetWorkflowError())
}

func TestCreateExpectedPowerShelfTestSuite(t *testing.T) {
	suite.Run(t, new(CreateExpectedPowerShelfTestSuite))
}

type UpdateExpectedPowerShelfTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (uepsts *UpdateExpectedPowerShelfTestSuite) SetupTest() {
	uepsts.env = uepsts.NewTestWorkflowEnvironment()
}

func (uepsts *UpdateExpectedPowerShelfTestSuite) AfterTest(suiteName, testName string) {
	uepsts.env.AssertExpectations(uepsts.T())
}

func (uepsts *UpdateExpectedPowerShelfTestSuite) Test_UpdateExpectedPowerShelf_Success() {
	var expectedPowerShelfManager iActivity.ManageExpectedPowerShelf

	request := &corev1.ExpectedPowerShelf{
		ExpectedPowerShelfId: &corev1.UUID{Value: "test-update-workflow-001"},
		BmcMacAddress:        "00:11:22:33:44:55",
		ShelfSerialNumber:    "SHELF-001",
	}

	// Mock UpdateExpectedPowerShelfOnSite activity
	uepsts.env.RegisterActivity(expectedPowerShelfManager.UpdateExpectedPowerShelfOnSite)
	uepsts.env.OnActivity(expectedPowerShelfManager.UpdateExpectedPowerShelfOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute workflow
	uepsts.env.ExecuteWorkflow(UpdateExpectedPowerShelf, request)
	uepsts.True(uepsts.env.IsWorkflowCompleted())
	uepsts.NoError(uepsts.env.GetWorkflowError())
}

func (uepsts *UpdateExpectedPowerShelfTestSuite) Test_UpdateExpectedPowerShelf_Failure() {
	var expectedPowerShelfManager iActivity.ManageExpectedPowerShelf

	request := &corev1.ExpectedPowerShelf{
		ExpectedPowerShelfId: &corev1.UUID{Value: "test-update-workflow-001"},
		BmcMacAddress:        "00:11:22:33:44:55",
		ShelfSerialNumber:    "SHELF-001",
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateExpectedPowerShelfOnSite activity
	uepsts.env.RegisterActivity(expectedPowerShelfManager.UpdateExpectedPowerShelfOnSite)
	uepsts.env.OnActivity(expectedPowerShelfManager.UpdateExpectedPowerShelfOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateExpectedPowerShelf workflow
	uepsts.env.ExecuteWorkflow(UpdateExpectedPowerShelf, request)
	uepsts.True(uepsts.env.IsWorkflowCompleted())
	uepsts.Error(uepsts.env.GetWorkflowError())
}

func TestUpdateExpectedPowerShelfTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateExpectedPowerShelfTestSuite))
}

type DeleteExpectedPowerShelfTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (depsts *DeleteExpectedPowerShelfTestSuite) SetupTest() {
	depsts.env = depsts.NewTestWorkflowEnvironment()
}

func (depsts *DeleteExpectedPowerShelfTestSuite) AfterTest(suiteName, testName string) {
	depsts.env.AssertExpectations(depsts.T())
}

func (depsts *DeleteExpectedPowerShelfTestSuite) Test_DeleteExpectedPowerShelf_Success() {
	var expectedPowerShelfManager iActivity.ManageExpectedPowerShelf

	request := &corev1.ExpectedPowerShelfRequest{
		ExpectedPowerShelfId: &corev1.UUID{Value: "test-delete-workflow-001"},
		BmcMacAddress:        "00:11:22:33:44:55",
	}

	// Mock DeleteExpectedPowerShelfOnSite activity
	depsts.env.RegisterActivity(expectedPowerShelfManager.DeleteExpectedPowerShelfOnSite)
	depsts.env.OnActivity(expectedPowerShelfManager.DeleteExpectedPowerShelfOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	depsts.env.ExecuteWorkflow(DeleteExpectedPowerShelf, request)
	depsts.True(depsts.env.IsWorkflowCompleted())
	depsts.NoError(depsts.env.GetWorkflowError())
}

func (depsts *DeleteExpectedPowerShelfTestSuite) Test_DeleteExpectedPowerShelf_Failure() {
	var expectedPowerShelfManager iActivity.ManageExpectedPowerShelf

	request := &corev1.ExpectedPowerShelfRequest{
		ExpectedPowerShelfId: &corev1.UUID{Value: "test-delete-workflow-001"},
		BmcMacAddress:        "00:11:22:33:44:55",
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteExpectedPowerShelfOnSite activity
	depsts.env.RegisterActivity(expectedPowerShelfManager.DeleteExpectedPowerShelfOnSite)
	depsts.env.OnActivity(expectedPowerShelfManager.DeleteExpectedPowerShelfOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteExpectedPowerShelf workflow
	depsts.env.ExecuteWorkflow(DeleteExpectedPowerShelf, request)
	depsts.True(depsts.env.IsWorkflowCompleted())
	depsts.Error(depsts.env.GetWorkflowError())
}

func TestDeleteExpectedPowerShelfTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteExpectedPowerShelfTestSuite))
}
