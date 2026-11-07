// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
)

type InventoryVpcPrefixTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ivts *InventoryVpcPrefixTestSuite) SetupTest() {
	ivts.env = ivts.NewTestWorkflowEnvironment()
}

func (ivts *InventoryVpcPrefixTestSuite) AfterTest(suiteName, testName string) {
	ivts.env.AssertExpectations(ivts.T())
}

func (ivts *InventoryVpcPrefixTestSuite) Test_DiscoverVpcPrefixInventory_Success() {
	var inventoryManager iActivity.ManageVpcPrefixInventory

	ivts.env.RegisterActivity(inventoryManager.DiscoverVpcPrefixInventory)
	ivts.env.OnActivity(inventoryManager.DiscoverVpcPrefixInventory, mock.Anything).Return(nil)

	// execute workflow
	ivts.env.ExecuteWorkflow(DiscoverVpcPrefixInventory)
	ivts.True(ivts.env.IsWorkflowCompleted())
	ivts.NoError(ivts.env.GetWorkflowError())
}

func (ivts *InventoryVpcPrefixTestSuite) Test_DiscoverVpcPrefixInventory_ActivityFails() {
	var inventoryManager iActivity.ManageVpcPrefixInventory

	errMsg := "Site Controller communication error"

	ivts.env.RegisterActivity(inventoryManager.DiscoverVpcPrefixInventory)
	ivts.env.OnActivity(inventoryManager.DiscoverVpcPrefixInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	ivts.env.ExecuteWorkflow(DiscoverVpcPrefixInventory)
	ivts.True(ivts.env.IsWorkflowCompleted())
	err := ivts.env.GetWorkflowError()
	ivts.Error(err)

	var applicationErr *temporal.ApplicationError
	ivts.True(errors.As(err, &applicationErr))
	ivts.Equal(errMsg, applicationErr.Error())
}

func TestInventoryVpcPrefixTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryVpcPrefixTestSuite))
}

type CreateVpcPrefixTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cvvts *CreateVpcPrefixTestSuite) SetupTest() {
	cvvts.env = cvvts.NewTestWorkflowEnvironment()
}

func (cvvts *CreateVpcPrefixTestSuite) AfterTest(suiteName, testName string) {
	cvvts.env.AssertExpectations(cvvts.T())
}

func (cvvts *CreateVpcPrefixTestSuite) Test_CreateVpcPrefix_Success() {
	var VpcPrefixManager iActivity.ManageVpcPrefix

	request := &corev1.VpcPrefixCreationRequest{
		Id:     &corev1.VpcPrefixId{Value: uuid.NewString()},
		Name:   "the_name",
		Prefix: "192.110.0.0/24",
		VpcId:  &corev1.VpcId{Value: uuid.NewString()},
	}

	// Mock CreateVpcPrefixOnSite activity
	cvvts.env.RegisterActivity(VpcPrefixManager.CreateVpcPrefixOnSite)
	cvvts.env.OnActivity(VpcPrefixManager.CreateVpcPrefixOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute CreateVpcPrefix workflow
	cvvts.env.ExecuteWorkflow(CreateVpcPrefix, request)
	cvvts.True(cvvts.env.IsWorkflowCompleted())
	cvvts.NoError(cvvts.env.GetWorkflowError())
}

func (cvvts *CreateVpcPrefixTestSuite) Test_CreateVpcPrefix_Failure() {
	var VpcPrefixManager iActivity.ManageVpcPrefix

	request := &corev1.VpcPrefixCreationRequest{
		Id:     &corev1.VpcPrefixId{Value: uuid.NewString()},
		Name:   "the_name",
		Prefix: "192.110.0.0/24",
		VpcId:  &corev1.VpcId{Value: uuid.NewString()},
	}

	errMsg := "Site Controller communication error"

	// Mock CreateVpcPrefixOnSite activity
	cvvts.env.RegisterActivity(VpcPrefixManager.CreateVpcPrefixOnSite)
	cvvts.env.OnActivity(VpcPrefixManager.CreateVpcPrefixOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute CreateVpcPrefix workflow
	cvvts.env.ExecuteWorkflow(CreateVpcPrefix, request)
	cvvts.True(cvvts.env.IsWorkflowCompleted())
	cvvts.Error(cvvts.env.GetWorkflowError())
}

func TestCreateVpcPrefixTestSuite(t *testing.T) {
	suite.Run(t, new(CreateVpcPrefixTestSuite))
}

type UpdateVpcPrefixTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (uvvts *UpdateVpcPrefixTestSuite) SetupTest() {
	uvvts.env = uvvts.NewTestWorkflowEnvironment()
}

func (uvvts *UpdateVpcPrefixTestSuite) AfterTest(suiteName, testName string) {
	uvvts.env.AssertExpectations(uvvts.T())
}

func (uvvts *UpdateVpcPrefixTestSuite) Test_UpdateVpcPrefix_Success() {
	var VpcPrefixManager iActivity.ManageVpcPrefix

	request := &corev1.VpcPrefixUpdateRequest{
		Id:   &corev1.VpcPrefixId{Value: uuid.NewString()},
		Name: util.GetStrPtr("updated-vpcprefix-name"),
	}

	// Mock UpdateVpcPrefixOnSite activity
	uvvts.env.RegisterActivity(VpcPrefixManager.UpdateVpcPrefixOnSite)
	uvvts.env.OnActivity(VpcPrefixManager.UpdateVpcPrefixOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	uvvts.env.ExecuteWorkflow(UpdateVpcPrefix, request)
	uvvts.True(uvvts.env.IsWorkflowCompleted())
	uvvts.NoError(uvvts.env.GetWorkflowError())
}

func (uvvts *UpdateVpcPrefixTestSuite) Test_UpdateVpcPrefix_Failure() {
	var VpcPrefixManager iActivity.ManageVpcPrefix

	request := &corev1.VpcPrefixUpdateRequest{
		Id:   &corev1.VpcPrefixId{Value: uuid.NewString()},
		Name: util.GetStrPtr("updated-vpcprefix-name"),
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateVpcPrefixOnSite activity
	uvvts.env.RegisterActivity(VpcPrefixManager.UpdateVpcPrefixOnSite)
	uvvts.env.OnActivity(VpcPrefixManager.UpdateVpcPrefixOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateVpcPrefix workflow
	uvvts.env.ExecuteWorkflow(UpdateVpcPrefix, request)
	uvvts.True(uvvts.env.IsWorkflowCompleted())
	uvvts.Error(uvvts.env.GetWorkflowError())
}

func TestUpdateVpcPrefixTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateVpcPrefixTestSuite))
}

type DeleteVpcPrefixTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cvvts *DeleteVpcPrefixTestSuite) SetupTest() {
	cvvts.env = cvvts.NewTestWorkflowEnvironment()
}

func (cvvts *DeleteVpcPrefixTestSuite) AfterTest(suiteName, testName string) {
	cvvts.env.AssertExpectations(cvvts.T())
}

func (cvvts *DeleteVpcPrefixTestSuite) Test_DeleteVpcPrefix_Success() {
	var VpcPrefixManager iActivity.ManageVpcPrefix

	request := &corev1.VpcPrefixDeletionRequest{
		Id: &corev1.VpcPrefixId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
	}

	// Mock DeleteVpcPrefixOnSite activity
	cvvts.env.RegisterActivity(VpcPrefixManager.DeleteVpcPrefixOnSite)
	cvvts.env.OnActivity(VpcPrefixManager.DeleteVpcPrefixOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	cvvts.env.ExecuteWorkflow(DeleteVpcPrefix, request)
	cvvts.True(cvvts.env.IsWorkflowCompleted())
	cvvts.NoError(cvvts.env.GetWorkflowError())
}

func (cvvts *DeleteVpcPrefixTestSuite) Test_DeleteVpcPrefix_Failure() {
	var VpcPrefixManager iActivity.ManageVpcPrefix

	request := &corev1.VpcPrefixDeletionRequest{
		Id: &corev1.VpcPrefixId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteVpcPrefixOnSite activity
	cvvts.env.RegisterActivity(VpcPrefixManager.DeleteVpcPrefixOnSite)
	cvvts.env.OnActivity(VpcPrefixManager.DeleteVpcPrefixOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteMachineInventory workflow
	cvvts.env.ExecuteWorkflow(DeleteVpcPrefix, request)
	cvvts.True(cvvts.env.IsWorkflowCompleted())
	cvvts.Error(cvvts.env.GetWorkflowError())
}

func TestDeleteVpcPrefixTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteVpcPrefixTestSuite))
}
