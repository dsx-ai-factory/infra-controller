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

type InventoryExpectedMachineTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *InventoryExpectedMachineTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *InventoryExpectedMachineTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *InventoryExpectedMachineTestSuite) Test_DiscoverExpectedMachineInventory_Success() {
	var inventoryManager iActivity.ManageExpectedMachineInventory

	s.env.RegisterActivity(inventoryManager.DiscoverExpectedMachineInventory)
	s.env.OnActivity(inventoryManager.DiscoverExpectedMachineInventory, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DiscoverExpectedMachineInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *InventoryExpectedMachineTestSuite) Test_DiscoverExpectedMachineInventory_ActivityFails() {
	var inventoryManager iActivity.ManageExpectedMachineInventory

	errMsg := "Site Controller communication error"

	s.env.RegisterActivity(inventoryManager.DiscoverExpectedMachineInventory)
	s.env.OnActivity(inventoryManager.DiscoverExpectedMachineInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	s.env.ExecuteWorkflow(DiscoverExpectedMachineInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestInventoryExpectedMachineTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryExpectedMachineTestSuite))
}

type CreateExpectedMachineTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cemts *CreateExpectedMachineTestSuite) SetupTest() {
	cemts.env = cemts.NewTestWorkflowEnvironment()
}

func (cemts *CreateExpectedMachineTestSuite) AfterTest(suiteName, testName string) {
	cemts.env.AssertExpectations(cemts.T())
}

func (cemts *CreateExpectedMachineTestSuite) Test_CreateExpectedMachine_Success() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.ExpectedMachine{
		Id:            &corev1.UUID{Value: "test-create-workflow-001"},
		BmcMacAddress: "00:11:22:33:44:55",
	}

	// Mock CreateExpectedMachineOnSite activity
	cemts.env.RegisterActivity(expectedMachineManager.CreateExpectedMachineOnSite)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachineOnSite, mock.Anything, mock.Anything).Return(nil)

	cemts.env.OnGetVersion(
		removeCreateExpectedMachineOnFlowChangeID,
		temporalworkflow.DefaultVersion,
		removeCreateExpectedMachineOnFlowVersion,
	).Return(removeCreateExpectedMachineOnFlowVersion)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachineOnFlow, mock.Anything, mock.Anything).Return(nil).Maybe()

	// Execute CreateExpectedMachine workflow
	cemts.env.ExecuteWorkflow(CreateExpectedMachine, request)
	cemts.True(cemts.env.IsWorkflowCompleted())
	cemts.NoError(cemts.env.GetWorkflowError())
	cemts.env.AssertActivityNumberOfCalls(cemts.T(), "CreateExpectedMachineOnFlow", 0)
}

func (cemts *CreateExpectedMachineTestSuite) Test_CreateExpectedMachine_Failure() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.ExpectedMachine{
		Id:            &corev1.UUID{Value: "test-create-workflow-001"},
		BmcMacAddress: "00:11:22:33:44:55",
	}

	errMsg := "Site Controller communication error"

	// Mock CreateExpectedMachineOnSite activity
	cemts.env.RegisterActivity(expectedMachineManager.CreateExpectedMachineOnSite)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachineOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute CreateExpectedMachine workflow
	cemts.env.ExecuteWorkflow(CreateExpectedMachine, request)
	cemts.True(cemts.env.IsWorkflowCompleted())
	cemts.Error(cemts.env.GetWorkflowError())
}

func (cemts *CreateExpectedMachineTestSuite) Test_CreateExpectedMachine_LegacyVersion_FlowFailure() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.ExpectedMachine{
		Id:            &corev1.UUID{Value: "test-create-workflow-001"},
		BmcMacAddress: "00:11:22:33:44:55",
	}

	// Mock CreateExpectedMachineOnSite activity to succeed
	cemts.env.RegisterActivity(expectedMachineManager.CreateExpectedMachineOnSite)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachineOnSite, mock.Anything, mock.Anything).Return(nil)

	cemts.env.OnGetVersion(
		removeCreateExpectedMachineOnFlowChangeID,
		temporalworkflow.DefaultVersion,
		removeCreateExpectedMachineOnFlowVersion,
	).Return(temporalworkflow.DefaultVersion)

	// Mock CreateExpectedMachineOnFlow activity to fail (best-effort, should not fail the workflow)
	cemts.env.RegisterActivity(expectedMachineManager.CreateExpectedMachineOnFlow)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachineOnFlow, mock.Anything, mock.Anything).Return(errors.New("Flow communication error"))

	// Execute CreateExpectedMachine workflow
	cemts.env.ExecuteWorkflow(CreateExpectedMachine, request)
	cemts.True(cemts.env.IsWorkflowCompleted())
	cemts.NoError(cemts.env.GetWorkflowError())
}

func TestCreateExpectedMachineTestSuite(t *testing.T) {
	suite.Run(t, new(CreateExpectedMachineTestSuite))
}

type UpdateExpectedMachineTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (uemts *UpdateExpectedMachineTestSuite) SetupTest() {
	uemts.env = uemts.NewTestWorkflowEnvironment()
}

func (uemts *UpdateExpectedMachineTestSuite) AfterTest(suiteName, testName string) {
	uemts.env.AssertExpectations(uemts.T())
}

func (uemts *UpdateExpectedMachineTestSuite) Test_UpdateExpectedMachine_Success() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.ExpectedMachine{
		Id:            &corev1.UUID{Value: "test-create-workflow-001"},
		BmcMacAddress: "00:11:22:33:44:55",
	}

	// Mock UpdateExpectedMachineOnSite activity
	uemts.env.RegisterActivity(expectedMachineManager.UpdateExpectedMachineOnSite)
	uemts.env.OnActivity(expectedMachineManager.UpdateExpectedMachineOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute workflow
	uemts.env.ExecuteWorkflow(UpdateExpectedMachine, request)
	uemts.True(uemts.env.IsWorkflowCompleted())
	uemts.NoError(uemts.env.GetWorkflowError())
}

func (uemts *UpdateExpectedMachineTestSuite) Test_UpdateExpectedMachine_Failure() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.ExpectedMachine{
		Id:            &corev1.UUID{Value: "test-create-workflow-001"},
		BmcMacAddress: "00:11:22:33:44:55",
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateExpectedMachineOnSite activity
	uemts.env.RegisterActivity(expectedMachineManager.UpdateExpectedMachineOnSite)
	uemts.env.OnActivity(expectedMachineManager.UpdateExpectedMachineOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateExpectedMachine workflow
	uemts.env.ExecuteWorkflow(UpdateExpectedMachine, request)
	uemts.True(uemts.env.IsWorkflowCompleted())
	uemts.Error(uemts.env.GetWorkflowError())
}

func TestUpdateExpectedMachineTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateExpectedMachineTestSuite))
}

type DeleteExpectedMachineTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (demts *DeleteExpectedMachineTestSuite) SetupTest() {
	demts.env = demts.NewTestWorkflowEnvironment()
}

func (demts *DeleteExpectedMachineTestSuite) AfterTest(suiteName, testName string) {
	demts.env.AssertExpectations(demts.T())
}

func (demts *DeleteExpectedMachineTestSuite) Test_DeleteExpectedMachine_Success() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.ExpectedMachineRequest{
		Id:            &corev1.UUID{Value: "test-delete-workflow-001"},
		BmcMacAddress: "00:11:22:33:44:55",
	}

	// Mock DeleteExpectedMachineOnSite activity
	demts.env.RegisterActivity(expectedMachineManager.DeleteExpectedMachineOnSite)
	demts.env.OnActivity(expectedMachineManager.DeleteExpectedMachineOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	demts.env.ExecuteWorkflow(DeleteExpectedMachine, request)
	demts.True(demts.env.IsWorkflowCompleted())
	demts.NoError(demts.env.GetWorkflowError())
}

func (demts *DeleteExpectedMachineTestSuite) Test_DeleteExpectedMachine_Failure() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.ExpectedMachineRequest{
		Id:            &corev1.UUID{Value: "test-delete-workflow-001"},
		BmcMacAddress: "00:11:22:33:44:55",
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteExpectedMachineOnSite activity
	demts.env.RegisterActivity(expectedMachineManager.DeleteExpectedMachineOnSite)
	demts.env.OnActivity(expectedMachineManager.DeleteExpectedMachineOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteExpectedMachine workflow
	demts.env.ExecuteWorkflow(DeleteExpectedMachine, request)
	demts.True(demts.env.IsWorkflowCompleted())
	demts.Error(demts.env.GetWorkflowError())
}

func TestDeleteExpectedMachineTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteExpectedMachineTestSuite))
}

type CreateExpectedMachinesTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cemts *CreateExpectedMachinesTestSuite) SetupTest() {
	cemts.env = cemts.NewTestWorkflowEnvironment()
}

func (cemts *CreateExpectedMachinesTestSuite) AfterTest(suiteName, testName string) {
	cemts.env.AssertExpectations(cemts.T())
}

func (cemts *CreateExpectedMachinesTestSuite) Test_CreateExpectedMachines_Success() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.BatchExpectedMachineOperationRequest{
		ExpectedMachines: &corev1.ExpectedMachineList{
			ExpectedMachines: []*corev1.ExpectedMachine{
				{
					Id:                  &corev1.UUID{Value: "test-batch-create-001"},
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "SN001",
				},
				{
					Id:                  &corev1.UUID{Value: "test-batch-create-002"},
					BmcMacAddress:       "00:11:22:33:44:66",
					ChassisSerialNumber: "SN002",
				},
			},
		},
		AcceptPartialResults: true,
	}

	firstMachine := request.GetExpectedMachines().GetExpectedMachines()[0]
	secondMachine := request.GetExpectedMachines().GetExpectedMachines()[1]

	expectedResponse := &corev1.BatchExpectedMachineOperationResponse{
		Results: []*corev1.ExpectedMachineOperationResult{
			{
				Id:              firstMachine.GetId(),
				Success:         true,
				ExpectedMachine: firstMachine,
			},
			{
				Id:              secondMachine.GetId(),
				Success:         true,
				ExpectedMachine: secondMachine,
			},
		},
	}

	// Mock CreateExpectedMachinesOnSite activity
	cemts.env.RegisterActivity(expectedMachineManager.CreateExpectedMachinesOnSite)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachinesOnSite, mock.Anything, mock.Anything).Return(expectedResponse, nil)

	cemts.env.OnGetVersion(
		removeBatchCreateExpectedMachinesOnFlowChangeID,
		temporalworkflow.DefaultVersion,
		removeBatchCreateExpectedMachinesOnFlowVersion,
	).Return(removeBatchCreateExpectedMachinesOnFlowVersion)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachinesOnFlow, mock.Anything, mock.Anything).Return(nil).Maybe()

	// Execute CreateExpectedMachines workflow
	cemts.env.ExecuteWorkflow(CreateExpectedMachines, request)
	cemts.True(cemts.env.IsWorkflowCompleted())
	cemts.NoError(cemts.env.GetWorkflowError())
	cemts.env.AssertActivityNumberOfCalls(cemts.T(), "CreateExpectedMachinesOnFlow", 0)

	var response corev1.BatchExpectedMachineOperationResponse
	err := cemts.env.GetWorkflowResult(&response)
	cemts.NoError(err)
	cemts.Equal(2, len(response.Results))
	cemts.True(response.Results[0].Success)
	cemts.True(response.Results[1].Success)
	cemts.NotNil(response.Results[0].GetExpectedMachine())
	cemts.NotNil(response.Results[0].GetExpectedMachine().GetId())
	cemts.Equal(firstMachine.GetId().GetValue(), response.Results[0].GetExpectedMachine().GetId().GetValue())
	cemts.Equal("00:11:22:33:44:55", response.Results[0].GetExpectedMachine().GetBmcMacAddress())
	cemts.NotNil(response.Results[1].GetExpectedMachine())
	cemts.NotNil(response.Results[1].GetExpectedMachine().GetId())
	cemts.Equal(secondMachine.GetId().GetValue(), response.Results[1].GetExpectedMachine().GetId().GetValue())
	cemts.Equal("00:11:22:33:44:66", response.Results[1].GetExpectedMachine().GetBmcMacAddress())
}

func (cemts *CreateExpectedMachinesTestSuite) Test_CreateExpectedMachines_PartialSuccess() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.BatchExpectedMachineOperationRequest{
		ExpectedMachines: &corev1.ExpectedMachineList{
			ExpectedMachines: []*corev1.ExpectedMachine{
				{
					Id:                  &corev1.UUID{Value: "test-batch-create-001"},
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "SN001",
				},
				{
					Id:                  &corev1.UUID{Value: "test-batch-create-002"},
					BmcMacAddress:       "00:11:22:33:44:66",
					ChassisSerialNumber: "SN002",
				},
			},
		},
		AcceptPartialResults: true,
	}

	firstMachine := request.GetExpectedMachines().GetExpectedMachines()[0]
	secondMachine := request.GetExpectedMachines().GetExpectedMachines()[1]
	duplicateMacMsg := "duplicate MAC address"

	expectedResponse := &corev1.BatchExpectedMachineOperationResponse{
		Results: []*corev1.ExpectedMachineOperationResult{
			{
				Id:              firstMachine.GetId(),
				Success:         true,
				ExpectedMachine: firstMachine,
			},
			{
				Id:              secondMachine.GetId(),
				Success:         false,
				ErrorMessage:    &duplicateMacMsg,
				ExpectedMachine: nil,
			},
		},
	}

	// Mock CreateExpectedMachinesOnSite activity
	cemts.env.RegisterActivity(expectedMachineManager.CreateExpectedMachinesOnSite)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachinesOnSite, mock.Anything, mock.Anything).Return(expectedResponse, nil)

	cemts.env.OnGetVersion(
		removeBatchCreateExpectedMachinesOnFlowChangeID,
		temporalworkflow.DefaultVersion,
		removeBatchCreateExpectedMachinesOnFlowVersion,
	).Return(removeBatchCreateExpectedMachinesOnFlowVersion)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachinesOnFlow, mock.Anything, mock.Anything).Return(nil).Maybe()

	// Execute CreateExpectedMachines workflow
	cemts.env.ExecuteWorkflow(CreateExpectedMachines, request)
	cemts.True(cemts.env.IsWorkflowCompleted())
	cemts.NoError(cemts.env.GetWorkflowError())
	cemts.env.AssertActivityNumberOfCalls(cemts.T(), "CreateExpectedMachinesOnFlow", 0)

	var response corev1.BatchExpectedMachineOperationResponse
	err := cemts.env.GetWorkflowResult(&response)
	cemts.NoError(err)
	cemts.Equal(2, len(response.Results))
	cemts.True(response.Results[0].Success)
	cemts.False(response.Results[1].Success)
	cemts.NotNil(response.Results[0].GetExpectedMachine())
	cemts.NotNil(response.Results[0].GetExpectedMachine().GetId())
	cemts.Equal(firstMachine.GetId().GetValue(), response.Results[0].GetExpectedMachine().GetId().GetValue())
	cemts.Equal("00:11:22:33:44:55", response.Results[0].GetExpectedMachine().GetBmcMacAddress())
	cemts.Nil(response.Results[1].GetExpectedMachine())
	cemts.Equal("duplicate MAC address", response.Results[1].GetErrorMessage())
}

func (cemts *CreateExpectedMachinesTestSuite) Test_CreateExpectedMachines_LegacyVersion_FlowFailure() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	machine := &corev1.ExpectedMachine{
		Id:                  &corev1.UUID{Value: "test-batch-create-001"},
		BmcMacAddress:       "00:11:22:33:44:55",
		ChassisSerialNumber: "SN001",
	}
	request := &corev1.BatchExpectedMachineOperationRequest{
		ExpectedMachines: &corev1.ExpectedMachineList{
			ExpectedMachines: []*corev1.ExpectedMachine{machine},
		},
		AcceptPartialResults: true,
	}
	expectedResponse := &corev1.BatchExpectedMachineOperationResponse{
		Results: []*corev1.ExpectedMachineOperationResult{
			{
				Id:              machine.GetId(),
				Success:         true,
				ExpectedMachine: machine,
			},
		},
	}

	cemts.env.RegisterActivity(expectedMachineManager.CreateExpectedMachinesOnSite)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachinesOnSite, mock.Anything, mock.Anything).Return(expectedResponse, nil)

	cemts.env.OnGetVersion(
		removeBatchCreateExpectedMachinesOnFlowChangeID,
		temporalworkflow.DefaultVersion,
		removeBatchCreateExpectedMachinesOnFlowVersion,
	).Return(temporalworkflow.DefaultVersion)

	cemts.env.RegisterActivity(expectedMachineManager.CreateExpectedMachinesOnFlow)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachinesOnFlow, mock.Anything, mock.Anything).Return(errors.New("Flow communication error"))

	cemts.env.ExecuteWorkflow(CreateExpectedMachines, request)
	cemts.True(cemts.env.IsWorkflowCompleted())
	cemts.NoError(cemts.env.GetWorkflowError())
}

func (cemts *CreateExpectedMachinesTestSuite) Test_CreateExpectedMachines_Failure() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.BatchExpectedMachineOperationRequest{
		ExpectedMachines: &corev1.ExpectedMachineList{
			ExpectedMachines: []*corev1.ExpectedMachine{
				{
					Id:                  &corev1.UUID{Value: "test-batch-create-001"},
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "SN001",
				},
			},
		},
		AcceptPartialResults: true,
	}

	errMsg := "Site Controller communication error"

	// Mock CreateExpectedMachinesOnSite activity
	cemts.env.RegisterActivity(expectedMachineManager.CreateExpectedMachinesOnSite)
	cemts.env.OnActivity(expectedMachineManager.CreateExpectedMachinesOnSite, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// execute CreateExpectedMachines workflow
	cemts.env.ExecuteWorkflow(CreateExpectedMachines, request)
	cemts.True(cemts.env.IsWorkflowCompleted())
	cemts.Error(cemts.env.GetWorkflowError())
}

func TestCreateExpectedMachinesTestSuite(t *testing.T) {
	suite.Run(t, new(CreateExpectedMachinesTestSuite))
}

type UpdateExpectedMachinesTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (uemts *UpdateExpectedMachinesTestSuite) SetupTest() {
	uemts.env = uemts.NewTestWorkflowEnvironment()
}

func (uemts *UpdateExpectedMachinesTestSuite) AfterTest(suiteName, testName string) {
	uemts.env.AssertExpectations(uemts.T())
}

func (uemts *UpdateExpectedMachinesTestSuite) Test_UpdateExpectedMachines_Success() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.BatchExpectedMachineOperationRequest{
		ExpectedMachines: &corev1.ExpectedMachineList{
			ExpectedMachines: []*corev1.ExpectedMachine{
				{
					Id:                  &corev1.UUID{Value: "test-batch-update-001"},
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "SN001",
				},
				{
					Id:                  &corev1.UUID{Value: "test-batch-update-002"},
					BmcMacAddress:       "00:11:22:33:44:66",
					ChassisSerialNumber: "SN002",
				},
			},
		},
		AcceptPartialResults: true,
	}

	firstMachine := request.GetExpectedMachines().GetExpectedMachines()[0]
	secondMachine := request.GetExpectedMachines().GetExpectedMachines()[1]

	expectedResponse := &corev1.BatchExpectedMachineOperationResponse{
		Results: []*corev1.ExpectedMachineOperationResult{
			{
				Id:              firstMachine.GetId(),
				Success:         true,
				ExpectedMachine: firstMachine,
			},
			{
				Id:              secondMachine.GetId(),
				Success:         true,
				ExpectedMachine: secondMachine,
			},
		},
	}

	// Mock UpdateExpectedMachinesOnSite activity
	uemts.env.RegisterActivity(expectedMachineManager.UpdateExpectedMachinesOnSite)
	uemts.env.OnActivity(expectedMachineManager.UpdateExpectedMachinesOnSite, mock.Anything, mock.Anything).Return(expectedResponse, nil)

	// Execute UpdateExpectedMachines workflow
	uemts.env.ExecuteWorkflow(UpdateExpectedMachines, request)
	uemts.True(uemts.env.IsWorkflowCompleted())
	uemts.NoError(uemts.env.GetWorkflowError())

	var response corev1.BatchExpectedMachineOperationResponse
	err := uemts.env.GetWorkflowResult(&response)
	uemts.NoError(err)
	uemts.Equal(2, len(response.Results))
	uemts.True(response.Results[0].Success)
	uemts.True(response.Results[1].Success)
	uemts.NotNil(response.Results[0].GetExpectedMachine())
	uemts.NotNil(response.Results[0].GetExpectedMachine().GetId())
	uemts.Equal(firstMachine.GetId().GetValue(), response.Results[0].GetExpectedMachine().GetId().GetValue())
	uemts.Equal("00:11:22:33:44:55", response.Results[0].GetExpectedMachine().GetBmcMacAddress())
	uemts.NotNil(response.Results[1].GetExpectedMachine())
	uemts.NotNil(response.Results[1].GetExpectedMachine().GetId())
	uemts.Equal(secondMachine.GetId().GetValue(), response.Results[1].GetExpectedMachine().GetId().GetValue())
	uemts.Equal("00:11:22:33:44:66", response.Results[1].GetExpectedMachine().GetBmcMacAddress())
}

func (uemts *UpdateExpectedMachinesTestSuite) Test_UpdateExpectedMachines_PartialSuccess() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.BatchExpectedMachineOperationRequest{
		ExpectedMachines: &corev1.ExpectedMachineList{
			ExpectedMachines: []*corev1.ExpectedMachine{
				{
					Id:                  &corev1.UUID{Value: "test-batch-update-001"},
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "SN001",
				},
				{
					Id:                  &corev1.UUID{Value: "test-batch-update-002"},
					BmcMacAddress:       "00:11:22:33:44:66",
					ChassisSerialNumber: "SN002",
				},
			},
		},
		AcceptPartialResults: true,
	}

	firstMachine := request.GetExpectedMachines().GetExpectedMachines()[0]
	secondMachine := request.GetExpectedMachines().GetExpectedMachines()[1]
	notFoundMsg := "machine not found"

	expectedResponse := &corev1.BatchExpectedMachineOperationResponse{
		Results: []*corev1.ExpectedMachineOperationResult{
			{
				Id:              firstMachine.GetId(),
				Success:         true,
				ExpectedMachine: firstMachine,
			},
			{
				Id:              secondMachine.GetId(),
				Success:         false,
				ErrorMessage:    &notFoundMsg,
				ExpectedMachine: nil,
			},
		},
	}

	// Mock UpdateExpectedMachinesOnSite activity
	uemts.env.RegisterActivity(expectedMachineManager.UpdateExpectedMachinesOnSite)
	uemts.env.OnActivity(expectedMachineManager.UpdateExpectedMachinesOnSite, mock.Anything, mock.Anything).Return(expectedResponse, nil)

	// Execute UpdateExpectedMachines workflow
	uemts.env.ExecuteWorkflow(UpdateExpectedMachines, request)
	uemts.True(uemts.env.IsWorkflowCompleted())
	uemts.NoError(uemts.env.GetWorkflowError())

	var response corev1.BatchExpectedMachineOperationResponse
	err := uemts.env.GetWorkflowResult(&response)
	uemts.NoError(err)
	uemts.Equal(2, len(response.Results))
	uemts.True(response.Results[0].Success)
	uemts.False(response.Results[1].Success)
	uemts.NotNil(response.Results[0].GetExpectedMachine())
	uemts.NotNil(response.Results[0].GetExpectedMachine().GetId())
	uemts.Equal(firstMachine.GetId().GetValue(), response.Results[0].GetExpectedMachine().GetId().GetValue())
	uemts.Equal("00:11:22:33:44:55", response.Results[0].GetExpectedMachine().GetBmcMacAddress())
	uemts.Nil(response.Results[1].GetExpectedMachine())
	uemts.Equal("machine not found", response.Results[1].GetErrorMessage())
}

func (uemts *UpdateExpectedMachinesTestSuite) Test_UpdateExpectedMachines_Failure() {
	var expectedMachineManager iActivity.ManageExpectedMachine

	request := &corev1.BatchExpectedMachineOperationRequest{
		ExpectedMachines: &corev1.ExpectedMachineList{
			ExpectedMachines: []*corev1.ExpectedMachine{
				{
					Id:                  &corev1.UUID{Value: "test-batch-update-001"},
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "SN001",
				},
			},
		},
		AcceptPartialResults: true,
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateExpectedMachinesOnSite activity
	uemts.env.RegisterActivity(expectedMachineManager.UpdateExpectedMachinesOnSite)
	uemts.env.OnActivity(expectedMachineManager.UpdateExpectedMachinesOnSite, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// execute UpdateExpectedMachines workflow
	uemts.env.ExecuteWorkflow(UpdateExpectedMachines, request)
	uemts.True(uemts.env.IsWorkflowCompleted())
	uemts.Error(uemts.env.GetWorkflowError())
}

func TestUpdateExpectedMachinesTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateExpectedMachinesTestSuite))
}
