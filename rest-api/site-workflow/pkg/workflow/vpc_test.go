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
)

type InventoryVpcTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ivts *InventoryVpcTestSuite) SetupTest() {
	ivts.env = ivts.NewTestWorkflowEnvironment()
}

func (ivts *InventoryVpcTestSuite) AfterTest(suiteName, testName string) {
	ivts.env.AssertExpectations(ivts.T())
}

func (ivts *InventoryVpcTestSuite) Test_DiscoverVPCInventory_Success() {
	var inventoryManager iActivity.ManageVPCInventory

	ivts.env.RegisterActivity(inventoryManager.DiscoverVPCInventory)
	ivts.env.OnActivity(inventoryManager.DiscoverVPCInventory, mock.Anything).Return(nil)

	// execute workflow
	ivts.env.ExecuteWorkflow(DiscoverVPCInventory)
	ivts.True(ivts.env.IsWorkflowCompleted())
	ivts.NoError(ivts.env.GetWorkflowError())
}

func (ivts *InventoryVpcTestSuite) Test_DiscoverVPCInventory_ActivityFails() {
	var inventoryManager iActivity.ManageVPCInventory

	errMsg := "Site Controller communication error"

	ivts.env.RegisterActivity(inventoryManager.DiscoverVPCInventory)
	ivts.env.OnActivity(inventoryManager.DiscoverVPCInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	ivts.env.ExecuteWorkflow(DiscoverVPCInventory)
	ivts.True(ivts.env.IsWorkflowCompleted())
	err := ivts.env.GetWorkflowError()
	ivts.Error(err)

	var applicationErr *temporal.ApplicationError
	ivts.True(errors.As(err, &applicationErr))
	ivts.Equal(errMsg, applicationErr.Error())
}

func TestInventoryVpcTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryVpcTestSuite))
}

type CreateVpcV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cvv2ts *CreateVpcV2TestSuite) SetupTest() {
	cvv2ts.env = cvv2ts.NewTestWorkflowEnvironment()
}

func (cvv2ts *CreateVpcV2TestSuite) AfterTest(suiteName, testName string) {
	cvv2ts.env.AssertExpectations(cvv2ts.T())
}

func (cvv2ts *CreateVpcV2TestSuite) Test_CreateVpcV2_Success() {
	var VpcManager iActivity.ManageVPC
	activeVni := uint32(7301)

	request := &corev1.VpcCreationRequest{
		Id:                   &corev1.VpcId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Name:                 "the_name",
		TenantOrganizationId: "the_org",
	}
	controllerVpc := &corev1.Vpc{
		Id:   request.Id,
		Name: request.Name,
		Status: &corev1.VpcStatus{
			Vni: &activeVni,
		},
	}

	// Mock CreateVpcOnSite activity
	cvv2ts.env.RegisterActivity(VpcManager.CreateVpcOnSite)
	cvv2ts.env.OnActivity(VpcManager.CreateVpcOnSite, mock.Anything, mock.Anything).Return(controllerVpc, nil)

	// Execute CreateVPCV2 workflow
	cvv2ts.env.ExecuteWorkflow(CreateVPCV2, request)
	cvv2ts.True(cvv2ts.env.IsWorkflowCompleted())
	cvv2ts.NoError(cvv2ts.env.GetWorkflowError())

	var result corev1.Vpc
	cvv2ts.NoError(cvv2ts.env.GetWorkflowResult(&result))
	cvv2ts.Equal(controllerVpc.Id.Value, result.Id.Value)
	cvv2ts.Equal(controllerVpc.Name, result.Name)
	cvv2ts.Equal(activeVni, result.GetStatus().GetVni())
}

func (cvv2ts *CreateVpcV2TestSuite) Test_CreateVpcV2_Failure() {
	var VpcManager iActivity.ManageVPC

	request := &corev1.VpcCreationRequest{
		Id:                   &corev1.VpcId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Name:                 "the_name",
		TenantOrganizationId: "the_org",
	}

	errMsg := "Site Controller communication error"

	// Mock CreateVpcOnSite activity
	cvv2ts.env.RegisterActivity(VpcManager.CreateVpcOnSite)
	cvv2ts.env.OnActivity(VpcManager.CreateVpcOnSite, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// execute CreateVPCV2 workflow
	cvv2ts.env.ExecuteWorkflow(CreateVPCV2, request)
	cvv2ts.True(cvv2ts.env.IsWorkflowCompleted())
	cvv2ts.Error(cvv2ts.env.GetWorkflowError())
}

func TestCreateVpcV2TestSuite(t *testing.T) {
	suite.Run(t, new(CreateVpcV2TestSuite))
}

type UpdateVpcTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (uvts *UpdateVpcTestSuite) SetupTest() {
	uvts.env = uvts.NewTestWorkflowEnvironment()
}

func (uvts *UpdateVpcTestSuite) AfterTest(suiteName, testName string) {
	uvts.env.AssertExpectations(uvts.T())
}

func (uvts *UpdateVpcTestSuite) Test_UpdateVpc_Success() {
	var VpcManager iActivity.ManageVPC

	request := &corev1.VpcUpdateRequest{
		Id:   &corev1.VpcId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Name: "the_name",
	}

	// Mock UpdateVpcOnSite activity
	uvts.env.RegisterActivity(VpcManager.UpdateVpcOnSite)
	uvts.env.OnActivity(VpcManager.UpdateVpcOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	uvts.env.ExecuteWorkflow(UpdateVPC, request)
	uvts.True(uvts.env.IsWorkflowCompleted())
	uvts.NoError(uvts.env.GetWorkflowError())
}

func (uvts *UpdateVpcTestSuite) Test_UpdateVpc_Failure() {
	var VpcManager iActivity.ManageVPC

	request := &corev1.VpcUpdateRequest{
		Id:   &corev1.VpcId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Name: "the_name",
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateVpcOnSite activity
	uvts.env.RegisterActivity(VpcManager.UpdateVpcOnSite)
	uvts.env.OnActivity(VpcManager.UpdateVpcOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateVPC workflow
	uvts.env.ExecuteWorkflow(UpdateVPC, request)
	uvts.True(uvts.env.IsWorkflowCompleted())
	uvts.Error(uvts.env.GetWorkflowError())
}

func TestUpdateVpcTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateVpcTestSuite))
}

type UpdateVpcVirtualizationTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (uvvts *UpdateVpcVirtualizationTestSuite) SetupTest() {
	uvvts.env = uvvts.NewTestWorkflowEnvironment()
}

func (uvvts *UpdateVpcVirtualizationTestSuite) AfterTest(suiteName, testName string) {
	uvvts.env.AssertExpectations(uvvts.T())
}

func (uvvts *UpdateVpcVirtualizationTestSuite) Test_UpdateVpcVirtualization_Success() {
	var VpcManager iActivity.ManageVPC

	request := &corev1.VpcUpdateVirtualizationRequest{
		Id:                        &corev1.VpcId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		NetworkVirtualizationType: corev1.VpcVirtualizationType_FNN.Enum(),
	}

	// Mock UpdateVpcOnSite activity
	uvvts.env.RegisterActivity(VpcManager.UpdateVpcVirtualizationOnSite)
	uvvts.env.OnActivity(VpcManager.UpdateVpcVirtualizationOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	uvvts.env.ExecuteWorkflow(UpdateVPCVirtualization, request)
	uvvts.True(uvvts.env.IsWorkflowCompleted())
	uvvts.NoError(uvvts.env.GetWorkflowError())
}

func (uvvts *UpdateVpcVirtualizationTestSuite) Test_UpdateVpcVirtualization_Failure() {
	var VpcManager iActivity.ManageVPC

	request := &corev1.VpcUpdateVirtualizationRequest{
		Id:                        &corev1.VpcId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		NetworkVirtualizationType: corev1.VpcVirtualizationType_FNN.Enum(),
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateVpcOnSite activity
	uvvts.env.RegisterActivity(VpcManager.UpdateVpcVirtualizationOnSite)
	uvvts.env.OnActivity(VpcManager.UpdateVpcVirtualizationOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateVPC workflow
	uvvts.env.ExecuteWorkflow(UpdateVPCVirtualization, request)
	uvvts.True(uvvts.env.IsWorkflowCompleted())
	uvvts.Error(uvvts.env.GetWorkflowError())
}

func TestUpdateVpcVirtualizationTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateVpcVirtualizationTestSuite))
}

type DeleteVpcV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cvv2ts *DeleteVpcV2TestSuite) SetupTest() {
	cvv2ts.env = cvv2ts.NewTestWorkflowEnvironment()
}

func (cvv2ts *DeleteVpcV2TestSuite) AfterTest(suiteName, testName string) {
	cvv2ts.env.AssertExpectations(cvv2ts.T())
}

func (cvv2ts *DeleteVpcV2TestSuite) Test_DeleteVpcV2_Success() {
	var VpcManager iActivity.ManageVPC

	request := &corev1.VpcDeletionRequest{
		Id: &corev1.VpcId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
	}

	// Mock DeleteVpcOnSite activity
	cvv2ts.env.RegisterActivity(VpcManager.DeleteVpcOnSite)
	cvv2ts.env.OnActivity(VpcManager.DeleteVpcOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	cvv2ts.env.ExecuteWorkflow(DeleteVPCV2, request)
	cvv2ts.True(cvv2ts.env.IsWorkflowCompleted())
	cvv2ts.NoError(cvv2ts.env.GetWorkflowError())
}

func (cvv2ts *DeleteVpcV2TestSuite) Test_DeleteVpcV2_Failure() {
	var VpcManager iActivity.ManageVPC

	request := &corev1.VpcDeletionRequest{
		Id: &corev1.VpcId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteVpcOnSite activity
	cvv2ts.env.RegisterActivity(VpcManager.DeleteVpcOnSite)
	cvv2ts.env.OnActivity(VpcManager.DeleteVpcOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteMachineInventory workflow
	cvv2ts.env.ExecuteWorkflow(DeleteVPCV2, request)
	cvv2ts.True(cvv2ts.env.IsWorkflowCompleted())
	cvv2ts.Error(cvv2ts.env.GetWorkflowError())
}

func TestDeleteVpcV2TestSuite(t *testing.T) {
	suite.Run(t, new(DeleteVpcV2TestSuite))
}
