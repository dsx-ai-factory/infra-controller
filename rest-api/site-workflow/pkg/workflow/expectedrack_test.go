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
	"go.temporal.io/sdk/testsuite"
	temporalworkflow "go.temporal.io/sdk/workflow"
)

type CreateExpectedRackTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (certs *CreateExpectedRackTestSuite) SetupTest() {
	certs.env = certs.NewTestWorkflowEnvironment()
}

func (certs *CreateExpectedRackTestSuite) AfterTest(suiteName, testName string) {
	certs.env.AssertExpectations(certs.T())
}

func (certs *CreateExpectedRackTestSuite) Test_CreateExpectedRack_Success() {
	var expectedRackManager iActivity.ManageExpectedRack

	request := &corev1.ExpectedRack{
		RackId:        &corev1.RackId{Id: "test-create-rack-workflow-001"},
		RackProfileId: &corev1.RackProfileId{Id: "test-create-rack-profile-001"},
	}

	// Mock CreateExpectedRackOnSite activity
	certs.env.RegisterActivity(expectedRackManager.CreateExpectedRackOnSite)
	certs.env.OnActivity(expectedRackManager.CreateExpectedRackOnSite, mock.Anything, mock.Anything).Return(nil)

	certs.env.OnGetVersion(
		removeCreateExpectedRackOnFlowChangeID,
		temporalworkflow.DefaultVersion,
		removeCreateExpectedRackOnFlowVersion,
	).Return(removeCreateExpectedRackOnFlowVersion)
	certs.env.OnActivity(expectedRackManager.CreateExpectedRackOnFlow, mock.Anything, mock.Anything).Return(nil).Maybe()

	// Execute CreateExpectedRack workflow
	certs.env.ExecuteWorkflow(CreateExpectedRack, request)
	certs.True(certs.env.IsWorkflowCompleted())
	certs.NoError(certs.env.GetWorkflowError())
	certs.env.AssertActivityNumberOfCalls(certs.T(), "CreateExpectedRackOnFlow", 0)
}

func (certs *CreateExpectedRackTestSuite) Test_CreateExpectedRack_Failure() {
	var expectedRackManager iActivity.ManageExpectedRack

	request := &corev1.ExpectedRack{
		RackId:        &corev1.RackId{Id: "test-create-rack-workflow-001"},
		RackProfileId: &corev1.RackProfileId{Id: "test-create-rack-profile-001"},
	}

	errMsg := "Site Controller communication error"

	// Mock CreateExpectedRackOnSite activity
	certs.env.RegisterActivity(expectedRackManager.CreateExpectedRackOnSite)
	certs.env.OnActivity(expectedRackManager.CreateExpectedRackOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// Execute CreateExpectedRack workflow
	certs.env.ExecuteWorkflow(CreateExpectedRack, request)
	certs.True(certs.env.IsWorkflowCompleted())
	certs.Error(certs.env.GetWorkflowError())
}

func (certs *CreateExpectedRackTestSuite) Test_CreateExpectedRack_LegacyVersion_FlowFailure() {
	var expectedRackManager iActivity.ManageExpectedRack

	request := &corev1.ExpectedRack{
		RackId:        &corev1.RackId{Id: "test-create-rack-workflow-002"},
		RackProfileId: &corev1.RackProfileId{Id: "test-create-rack-profile-002"},
	}

	// Mock CreateExpectedRackOnSite activity (success)
	certs.env.RegisterActivity(expectedRackManager.CreateExpectedRackOnSite)
	certs.env.OnActivity(expectedRackManager.CreateExpectedRackOnSite, mock.Anything, mock.Anything).Return(nil)

	certs.env.OnGetVersion(
		removeCreateExpectedRackOnFlowChangeID,
		temporalworkflow.DefaultVersion,
		removeCreateExpectedRackOnFlowVersion,
	).Return(temporalworkflow.DefaultVersion)

	// Mock CreateExpectedRackOnFlow activity (failure - workflow should still succeed)
	certs.env.RegisterActivity(expectedRackManager.CreateExpectedRackOnFlow)
	certs.env.OnActivity(expectedRackManager.CreateExpectedRackOnFlow, mock.Anything, mock.Anything).Return(errors.New("Flow unavailable"))

	// Execute CreateExpectedRack workflow
	certs.env.ExecuteWorkflow(CreateExpectedRack, request)
	certs.True(certs.env.IsWorkflowCompleted())
	certs.NoError(certs.env.GetWorkflowError())
}

func TestCreateExpectedRackTestSuite(t *testing.T) {
	suite.Run(t, new(CreateExpectedRackTestSuite))
}

type UpdateExpectedRackTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (uerts *UpdateExpectedRackTestSuite) SetupTest() {
	uerts.env = uerts.NewTestWorkflowEnvironment()
}

func (uerts *UpdateExpectedRackTestSuite) AfterTest(suiteName, testName string) {
	uerts.env.AssertExpectations(uerts.T())
}

func (uerts *UpdateExpectedRackTestSuite) Test_UpdateExpectedRack_Success() {
	var expectedRackManager iActivity.ManageExpectedRack

	request := &corev1.ExpectedRack{
		RackId:        &corev1.RackId{Id: "test-update-rack-workflow-001"},
		RackProfileId: &corev1.RackProfileId{Id: "test-update-rack-profile-001"},
	}

	// Mock UpdateExpectedRackOnSite activity
	uerts.env.RegisterActivity(expectedRackManager.UpdateExpectedRackOnSite)
	uerts.env.OnActivity(expectedRackManager.UpdateExpectedRackOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute workflow
	uerts.env.ExecuteWorkflow(UpdateExpectedRack, request)
	uerts.True(uerts.env.IsWorkflowCompleted())
	uerts.NoError(uerts.env.GetWorkflowError())
}

func (uerts *UpdateExpectedRackTestSuite) Test_UpdateExpectedRack_Failure() {
	var expectedRackManager iActivity.ManageExpectedRack

	request := &corev1.ExpectedRack{
		RackId:        &corev1.RackId{Id: "test-update-rack-workflow-001"},
		RackProfileId: &corev1.RackProfileId{Id: "test-update-rack-profile-001"},
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateExpectedRackOnSite activity
	uerts.env.RegisterActivity(expectedRackManager.UpdateExpectedRackOnSite)
	uerts.env.OnActivity(expectedRackManager.UpdateExpectedRackOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// Execute UpdateExpectedRack workflow
	uerts.env.ExecuteWorkflow(UpdateExpectedRack, request)
	uerts.True(uerts.env.IsWorkflowCompleted())
	uerts.Error(uerts.env.GetWorkflowError())
}

func TestUpdateExpectedRackTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateExpectedRackTestSuite))
}

type DeleteExpectedRackTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (derts *DeleteExpectedRackTestSuite) SetupTest() {
	derts.env = derts.NewTestWorkflowEnvironment()
}

func (derts *DeleteExpectedRackTestSuite) AfterTest(suiteName, testName string) {
	derts.env.AssertExpectations(derts.T())
}

func (derts *DeleteExpectedRackTestSuite) Test_DeleteExpectedRack_Success() {
	var expectedRackManager iActivity.ManageExpectedRack

	request := &corev1.ExpectedRackRequest{
		RackId: "test-delete-rack-workflow-001",
	}

	// Mock DeleteExpectedRackOnSite activity
	derts.env.RegisterActivity(expectedRackManager.DeleteExpectedRackOnSite)
	derts.env.OnActivity(expectedRackManager.DeleteExpectedRackOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute workflow
	derts.env.ExecuteWorkflow(DeleteExpectedRack, request)
	derts.True(derts.env.IsWorkflowCompleted())
	derts.NoError(derts.env.GetWorkflowError())
}

func (derts *DeleteExpectedRackTestSuite) Test_DeleteExpectedRack_Failure() {
	var expectedRackManager iActivity.ManageExpectedRack

	request := &corev1.ExpectedRackRequest{
		RackId: "test-delete-rack-workflow-001",
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteExpectedRackOnSite activity
	derts.env.RegisterActivity(expectedRackManager.DeleteExpectedRackOnSite)
	derts.env.OnActivity(expectedRackManager.DeleteExpectedRackOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// Execute DeleteExpectedRack workflow
	derts.env.ExecuteWorkflow(DeleteExpectedRack, request)
	derts.True(derts.env.IsWorkflowCompleted())
	derts.Error(derts.env.GetWorkflowError())
}

func TestDeleteExpectedRackTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteExpectedRackTestSuite))
}

type ReplaceAllExpectedRacksTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (rarts *ReplaceAllExpectedRacksTestSuite) SetupTest() {
	rarts.env = rarts.NewTestWorkflowEnvironment()
}

func (rarts *ReplaceAllExpectedRacksTestSuite) AfterTest(suiteName, testName string) {
	rarts.env.AssertExpectations(rarts.T())
}

func (rarts *ReplaceAllExpectedRacksTestSuite) Test_ReplaceAllExpectedRacks_Success() {
	var expectedRackManager iActivity.ManageExpectedRack

	request := &corev1.ExpectedRackList{
		ExpectedRacks: []*corev1.ExpectedRack{
			{
				RackId:        &corev1.RackId{Id: "test-replace-rack-workflow-001"},
				RackProfileId: &corev1.RackProfileId{Id: "test-replace-rack-profile-001"},
			},
		},
	}

	// Mock ReplaceAllExpectedRacksOnSite activity
	rarts.env.RegisterActivity(expectedRackManager.ReplaceAllExpectedRacksOnSite)
	rarts.env.OnActivity(expectedRackManager.ReplaceAllExpectedRacksOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute workflow
	rarts.env.ExecuteWorkflow(ReplaceAllExpectedRacks, request)
	rarts.True(rarts.env.IsWorkflowCompleted())
	rarts.NoError(rarts.env.GetWorkflowError())
}

func (rarts *ReplaceAllExpectedRacksTestSuite) Test_ReplaceAllExpectedRacks_Failure() {
	var expectedRackManager iActivity.ManageExpectedRack

	request := &corev1.ExpectedRackList{
		ExpectedRacks: []*corev1.ExpectedRack{
			{
				RackId:        &corev1.RackId{Id: "test-replace-rack-workflow-001"},
				RackProfileId: &corev1.RackProfileId{Id: "test-replace-rack-profile-001"},
			},
		},
	}

	errMsg := "Site Controller communication error"

	// Mock ReplaceAllExpectedRacksOnSite activity
	rarts.env.RegisterActivity(expectedRackManager.ReplaceAllExpectedRacksOnSite)
	rarts.env.OnActivity(expectedRackManager.ReplaceAllExpectedRacksOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// Execute ReplaceAllExpectedRacks workflow
	rarts.env.ExecuteWorkflow(ReplaceAllExpectedRacks, request)
	rarts.True(rarts.env.IsWorkflowCompleted())
	rarts.Error(rarts.env.GetWorkflowError())
}

func TestReplaceAllExpectedRacksTestSuite(t *testing.T) {
	suite.Run(t, new(ReplaceAllExpectedRacksTestSuite))
}

type DeleteAllExpectedRacksTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (darts *DeleteAllExpectedRacksTestSuite) SetupTest() {
	darts.env = darts.NewTestWorkflowEnvironment()
}

func (darts *DeleteAllExpectedRacksTestSuite) AfterTest(suiteName, testName string) {
	darts.env.AssertExpectations(darts.T())
}

func (darts *DeleteAllExpectedRacksTestSuite) Test_DeleteAllExpectedRacks_Success() {
	var expectedRackManager iActivity.ManageExpectedRack

	// Mock DeleteAllExpectedRacksOnSite activity
	darts.env.RegisterActivity(expectedRackManager.DeleteAllExpectedRacksOnSite)
	darts.env.OnActivity(expectedRackManager.DeleteAllExpectedRacksOnSite, mock.Anything).Return(nil)

	// Execute workflow
	darts.env.ExecuteWorkflow(DeleteAllExpectedRacks)
	darts.True(darts.env.IsWorkflowCompleted())
	darts.NoError(darts.env.GetWorkflowError())
}

func (darts *DeleteAllExpectedRacksTestSuite) Test_DeleteAllExpectedRacks_Failure() {
	var expectedRackManager iActivity.ManageExpectedRack

	errMsg := "Site Controller communication error"

	// Mock DeleteAllExpectedRacksOnSite activity
	darts.env.RegisterActivity(expectedRackManager.DeleteAllExpectedRacksOnSite)
	darts.env.OnActivity(expectedRackManager.DeleteAllExpectedRacksOnSite, mock.Anything).Return(errors.New(errMsg))

	// Execute DeleteAllExpectedRacks workflow
	darts.env.ExecuteWorkflow(DeleteAllExpectedRacks)
	darts.True(darts.env.IsWorkflowCompleted())
	darts.Error(darts.env.GetWorkflowError())
}

func TestDeleteAllExpectedRacksTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteAllExpectedRacksTestSuite))
}
