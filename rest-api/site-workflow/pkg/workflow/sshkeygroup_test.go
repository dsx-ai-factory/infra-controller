// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"

	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type InventorySSHKeyGroupTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ivts *InventorySSHKeyGroupTestSuite) SetupTest() {
	ivts.env = ivts.NewTestWorkflowEnvironment()
}

func (ivts *InventorySSHKeyGroupTestSuite) AfterTest(suiteName, testName string) {
	ivts.env.AssertExpectations(ivts.T())
}

func (ivts *InventorySSHKeyGroupTestSuite) Test_DiscoverSSHKeyGroupInventory_Success() {
	var inventoryManager iActivity.ManageSSHKeyGroupInventory

	ivts.env.RegisterActivity(inventoryManager.DiscoverSSHKeyGroupInventory)
	ivts.env.OnActivity(inventoryManager.DiscoverSSHKeyGroupInventory, mock.Anything).Return(nil)

	// execute workflow
	ivts.env.ExecuteWorkflow(DiscoverSSHKeyGroupInventory)
	ivts.True(ivts.env.IsWorkflowCompleted())
	ivts.NoError(ivts.env.GetWorkflowError())
}

func (ivts *InventorySSHKeyGroupTestSuite) Test_DiscoverSSHKeyGroupInventory_ActivityFails() {
	var inventoryManager iActivity.ManageSSHKeyGroupInventory

	errMsg := "Site Controller communication error"

	ivts.env.RegisterActivity(inventoryManager.DiscoverSSHKeyGroupInventory)
	ivts.env.OnActivity(inventoryManager.DiscoverSSHKeyGroupInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	ivts.env.ExecuteWorkflow(DiscoverSSHKeyGroupInventory)
	ivts.True(ivts.env.IsWorkflowCompleted())
	err := ivts.env.GetWorkflowError()
	ivts.Error(err)

	var applicationErr *temporal.ApplicationError
	ivts.True(errors.As(err, &applicationErr))
	ivts.Equal(errMsg, applicationErr.Error())
}

func TestInventorySSHKeyGroupTestSuite(t *testing.T) {
	suite.Run(t, new(InventorySSHKeyGroupTestSuite))
}

type CreateSSHKeyGroupV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cvv2ts *CreateSSHKeyGroupV2TestSuite) SetupTest() {
	cvv2ts.env = cvv2ts.NewTestWorkflowEnvironment()
}

func (cvv2ts *CreateSSHKeyGroupV2TestSuite) AfterTest(suiteName, testName string) {
	cvv2ts.env.AssertExpectations(cvv2ts.T())
}

func (cvv2ts *CreateSSHKeyGroupV2TestSuite) Test_CreateSSHKeyGroupV2_Success() {
	var sshKeyGroupManager iActivity.ManageSSHKeyGroup

	request := &corev1.CreateTenantKeysetRequest{
		KeysetIdentifier: &corev1.TenantKeysetIdentifier{
			KeysetId:       "the_id",
			OrganizationId: "the_org",
		},
		Version: "the_version",
	}

	// Mock CreateVpcOnSite activity
	cvv2ts.env.RegisterActivity(sshKeyGroupManager.CreateSSHKeyGroupOnSite)
	cvv2ts.env.OnActivity(sshKeyGroupManager.CreateSSHKeyGroupOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute CreateSSHKeyGroupV2 workflow
	cvv2ts.env.ExecuteWorkflow(CreateSSHKeyGroupV2, request)
	cvv2ts.True(cvv2ts.env.IsWorkflowCompleted())
	cvv2ts.NoError(cvv2ts.env.GetWorkflowError())
}

func (cvv2ts *CreateSSHKeyGroupV2TestSuite) Test_CreateSSHKeyGroupV2_Failure() {
	var sshKeyGroupManager iActivity.ManageSSHKeyGroup

	request := &corev1.CreateTenantKeysetRequest{
		KeysetIdentifier: &corev1.TenantKeysetIdentifier{
			KeysetId:       "the_id",
			OrganizationId: "the_org",
		},
		Version: "the_version",
	}

	errMsg := "Site Controller communication error"

	// Mock CreateVpcOnSite activity
	cvv2ts.env.RegisterActivity(sshKeyGroupManager.CreateSSHKeyGroupOnSite)
	cvv2ts.env.OnActivity(sshKeyGroupManager.CreateSSHKeyGroupOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute CreateSSHKeyGroupV2 workflow
	cvv2ts.env.ExecuteWorkflow(CreateSSHKeyGroupV2, request)
	cvv2ts.True(cvv2ts.env.IsWorkflowCompleted())
	cvv2ts.Error(cvv2ts.env.GetWorkflowError())
}

func TestCreateSSHKeyGroupV2TestSuite(t *testing.T) {
	suite.Run(t, new(CreateSSHKeyGroupV2TestSuite))
}

type UpdateSSHKeyGroupV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (uvts *UpdateSSHKeyGroupV2TestSuite) SetupTest() {
	uvts.env = uvts.NewTestWorkflowEnvironment()
}

func (uvts *UpdateSSHKeyGroupV2TestSuite) AfterTest(suiteName, testName string) {
	uvts.env.AssertExpectations(uvts.T())
}

func (uvts *UpdateSSHKeyGroupV2TestSuite) Test_UpdateSSHKeyGroupV2_Success() {
	var sshKeyGroupManager iActivity.ManageSSHKeyGroup

	request := &corev1.UpdateTenantKeysetRequest{
		KeysetIdentifier: &corev1.TenantKeysetIdentifier{
			KeysetId:       "the_id",
			OrganizationId: "the_org",
		},
		Version: "the_version",
	}

	// Mock UpdateVpcOnSite activity
	uvts.env.RegisterActivity(sshKeyGroupManager.UpdateSSHKeyGroupOnSite)
	uvts.env.OnActivity(sshKeyGroupManager.UpdateSSHKeyGroupOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	uvts.env.ExecuteWorkflow(UpdateSSHKeyGroupV2, request)
	uvts.True(uvts.env.IsWorkflowCompleted())
	uvts.NoError(uvts.env.GetWorkflowError())
}

func (uvts *UpdateSSHKeyGroupV2TestSuite) Test_UpdateSSHKeyGroupV2_Failure() {
	var sshKeyGroupManager iActivity.ManageSSHKeyGroup

	request := &corev1.UpdateTenantKeysetRequest{
		KeysetIdentifier: &corev1.TenantKeysetIdentifier{
			KeysetId:       "the_id",
			OrganizationId: "the_org",
		},
		Version: "the_version",
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateVpcOnSite activity
	uvts.env.RegisterActivity(sshKeyGroupManager.UpdateSSHKeyGroupOnSite)
	uvts.env.OnActivity(sshKeyGroupManager.UpdateSSHKeyGroupOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateVPC workflow
	uvts.env.ExecuteWorkflow(UpdateSSHKeyGroupV2, request)
	uvts.True(uvts.env.IsWorkflowCompleted())
	uvts.Error(uvts.env.GetWorkflowError())
}

func TestUpdateSSHKeyGroupV2TestSuite(t *testing.T) {
	suite.Run(t, new(UpdateSSHKeyGroupV2TestSuite))
}

type DeleteSSHKeyGroupV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cvv2ts *DeleteSSHKeyGroupV2TestSuite) SetupTest() {
	cvv2ts.env = cvv2ts.NewTestWorkflowEnvironment()
}

func (cvv2ts *DeleteSSHKeyGroupV2TestSuite) AfterTest(suiteName, testName string) {
	cvv2ts.env.AssertExpectations(cvv2ts.T())
}

func (cvv2ts *DeleteSSHKeyGroupV2TestSuite) Test_DeleteSSHKeyGroupV2_Success() {
	var sshKeyGroupManager iActivity.ManageSSHKeyGroup

	request := &corev1.DeleteTenantKeysetRequest{
		KeysetIdentifier: &corev1.TenantKeysetIdentifier{
			KeysetId:       "the_id",
			OrganizationId: "the_org",
		},
	}

	// Mock DeleteVpcOnSite activity
	cvv2ts.env.RegisterActivity(sshKeyGroupManager.DeleteSSHKeyGroupOnSite)
	cvv2ts.env.OnActivity(sshKeyGroupManager.DeleteSSHKeyGroupOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	cvv2ts.env.ExecuteWorkflow(DeleteSSHKeyGroupV2, request)
	cvv2ts.True(cvv2ts.env.IsWorkflowCompleted())
	cvv2ts.NoError(cvv2ts.env.GetWorkflowError())
}

func (cvv2ts *DeleteSSHKeyGroupV2TestSuite) Test_DeleteSSHKeyGroupV2_Failure() {
	var sshKeyGroupManager iActivity.ManageSSHKeyGroup

	request := &corev1.DeleteTenantKeysetRequest{
		KeysetIdentifier: &corev1.TenantKeysetIdentifier{
			KeysetId:       "the_id",
			OrganizationId: "the_org",
		},
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteSSHKeyGroupOnSite activity
	cvv2ts.env.RegisterActivity(sshKeyGroupManager.DeleteSSHKeyGroupOnSite)
	cvv2ts.env.OnActivity(sshKeyGroupManager.DeleteSSHKeyGroupOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteSSHKeyGroupV2 workflow
	cvv2ts.env.ExecuteWorkflow(DeleteSSHKeyGroupV2, request)
	cvv2ts.True(cvv2ts.env.IsWorkflowCompleted())
	cvv2ts.Error(cvv2ts.env.GetWorkflowError())
}

func TestDeleteSSHKeyGroupV2TestSuite(t *testing.T) {
	suite.Run(t, new(DeleteSSHKeyGroupV2TestSuite))
}
