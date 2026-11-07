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

type InventoryIBPartitionTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *InventoryIBPartitionTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *InventoryIBPartitionTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *InventoryIBPartitionTestSuite) Test_DiscoverInfiniBandPartitionInventory_Success() {
	var inventoryManager iActivity.ManageInfiniBandPartitionInventory

	s.env.RegisterActivity(inventoryManager.DiscoverInfiniBandPartitionInventory)
	s.env.OnActivity(inventoryManager.DiscoverInfiniBandPartitionInventory, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DiscoverInfiniBandPartitionInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *InventoryIBPartitionTestSuite) Test_DiscoverInfiniBandPartitionInventory_ActivityFails() {
	var inventoryManager iActivity.ManageInfiniBandPartitionInventory

	errMsg := "Site Controller communication error"

	s.env.RegisterActivity(inventoryManager.DiscoverInfiniBandPartitionInventory)
	s.env.OnActivity(inventoryManager.DiscoverInfiniBandPartitionInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	s.env.ExecuteWorkflow(DiscoverInfiniBandPartitionInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestInventoryIBPartitionTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryIBPartitionTestSuite))
}

type CreateIBPartitionV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cipbv2ts *CreateIBPartitionV2TestSuite) SetupTest() {
	cipbv2ts.env = cipbv2ts.NewTestWorkflowEnvironment()
}

func (cipbv2ts *CreateIBPartitionV2TestSuite) AfterTest(suiteName, testName string) {
	cipbv2ts.env.AssertExpectations(cipbv2ts.T())
}

func (cipbv2ts *CreateIBPartitionV2TestSuite) Test_CreateIBPartitionV2_Success() {
	var IBPartitionManager iActivity.ManageInfiniBandPartition

	request := &corev1.IBPartitionCreationRequest{
		Id: &corev1.IBPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Config: &corev1.IBPartitionConfig{
			Name:                 "the_name",
			TenantOrganizationId: "the_org",
		},
	}

	// Mock CreateInfiniBandPartitionOnSite activity
	cipbv2ts.env.RegisterActivity(IBPartitionManager.CreateInfiniBandPartitionOnSite)
	cipbv2ts.env.OnActivity(IBPartitionManager.CreateInfiniBandPartitionOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute CreateIBPartitionV2 workflow
	cipbv2ts.env.ExecuteWorkflow(CreateInfiniBandPartitionV2, request)
	cipbv2ts.True(cipbv2ts.env.IsWorkflowCompleted())
	cipbv2ts.NoError(cipbv2ts.env.GetWorkflowError())
}

func (cipbv2ts *CreateIBPartitionV2TestSuite) Test_CreateIBPartitionV2_Failure() {
	var IBPartitionManager iActivity.ManageInfiniBandPartition

	request := &corev1.IBPartitionCreationRequest{
		Id: &corev1.IBPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Config: &corev1.IBPartitionConfig{
			Name:                 "the_name",
			TenantOrganizationId: "the_org",
		},
	}

	errMsg := "Site Controller communication error"

	// Mock CreateInfiniBandPartitionOnSite activity
	cipbv2ts.env.RegisterActivity(IBPartitionManager.CreateInfiniBandPartitionOnSite)
	cipbv2ts.env.OnActivity(IBPartitionManager.CreateInfiniBandPartitionOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute CreateIBPartitionV2 workflow
	cipbv2ts.env.ExecuteWorkflow(CreateInfiniBandPartitionV2, request)
	cipbv2ts.True(cipbv2ts.env.IsWorkflowCompleted())
	cipbv2ts.Error(cipbv2ts.env.GetWorkflowError())
}

func TestCreateIBPartitionV2TestSuite(t *testing.T) {
	suite.Run(t, new(CreateIBPartitionV2TestSuite))
}

type DeleteIBPartitionV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cipbv2ts *DeleteIBPartitionV2TestSuite) SetupTest() {
	cipbv2ts.env = cipbv2ts.NewTestWorkflowEnvironment()
}

func (cipbv2ts *DeleteIBPartitionV2TestSuite) AfterTest(suiteName, testName string) {
	cipbv2ts.env.AssertExpectations(cipbv2ts.T())
}

func (cipbv2ts *DeleteIBPartitionV2TestSuite) Test_DeleteIBPartitionV2_Success() {
	var IBPartitionManager iActivity.ManageInfiniBandPartition

	request := &corev1.IBPartitionDeletionRequest{
		Id: &corev1.IBPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
	}

	// Mock DeleteInfiniBandPartitionOnSite activity
	cipbv2ts.env.RegisterActivity(IBPartitionManager.DeleteInfiniBandPartitionOnSite)
	cipbv2ts.env.OnActivity(IBPartitionManager.DeleteInfiniBandPartitionOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	cipbv2ts.env.ExecuteWorkflow(DeleteInfiniBandPartitionV2, request)
	cipbv2ts.True(cipbv2ts.env.IsWorkflowCompleted())
	cipbv2ts.NoError(cipbv2ts.env.GetWorkflowError())
}

func (cipbv2ts *DeleteIBPartitionV2TestSuite) Test_DeleteIBPartitionV2_Failure() {
	var IBPartitionManager iActivity.ManageInfiniBandPartition

	request := &corev1.IBPartitionDeletionRequest{
		Id: &corev1.IBPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteInfiniBandPartitionOnSite activity
	cipbv2ts.env.RegisterActivity(IBPartitionManager.DeleteInfiniBandPartitionOnSite)
	cipbv2ts.env.OnActivity(IBPartitionManager.DeleteInfiniBandPartitionOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteInfiniBandPartitionV2 workflow
	cipbv2ts.env.ExecuteWorkflow(DeleteInfiniBandPartitionV2, request)
	cipbv2ts.True(cipbv2ts.env.IsWorkflowCompleted())
	cipbv2ts.Error(cipbv2ts.env.GetWorkflowError())
}

func TestDeleteIBPartitionV2TestSuite(t *testing.T) {
	suite.Run(t, new(DeleteIBPartitionV2TestSuite))
}

type UpdateIBPartitionV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (uibpv2ts *UpdateIBPartitionV2TestSuite) SetupTest() {
	uibpv2ts.env = uibpv2ts.NewTestWorkflowEnvironment()
}

func (uibpv2ts *UpdateIBPartitionV2TestSuite) AfterTest(suiteName, testName string) {
	uibpv2ts.env.AssertExpectations(uibpv2ts.T())
}

func (uibpv2ts *UpdateIBPartitionV2TestSuite) Test_UpdateIBPartitionV2_Success() {
	var IBPartitionManager iActivity.ManageInfiniBandPartition

	request := &corev1.IBPartitionUpdateRequest{
		Id: &corev1.IBPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Config: &corev1.IBPartitionConfig{
			Name:                 "the_name",
			TenantOrganizationId: "the_org",
		},
	}

	uibpv2ts.env.RegisterActivity(IBPartitionManager.UpdateInfiniBandPartitionOnSite)
	uibpv2ts.env.OnActivity(IBPartitionManager.UpdateInfiniBandPartitionOnSite, mock.Anything, mock.Anything).Return(nil)

	uibpv2ts.env.ExecuteWorkflow(UpdateInfiniBandPartition, request)
	uibpv2ts.True(uibpv2ts.env.IsWorkflowCompleted())
	uibpv2ts.NoError(uibpv2ts.env.GetWorkflowError())
}

func (uibpv2ts *UpdateIBPartitionV2TestSuite) Test_UpdateIBPartitionV2_Failure() {
	var IBPartitionManager iActivity.ManageInfiniBandPartition

	request := &corev1.IBPartitionUpdateRequest{
		Id: &corev1.IBPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Config: &corev1.IBPartitionConfig{
			Name:                 "the_name",
			TenantOrganizationId: "the_org",
		},
	}

	errMsg := "Site Controller communication error"

	uibpv2ts.env.RegisterActivity(IBPartitionManager.UpdateInfiniBandPartitionOnSite)
	uibpv2ts.env.OnActivity(IBPartitionManager.UpdateInfiniBandPartitionOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	uibpv2ts.env.ExecuteWorkflow(UpdateInfiniBandPartition, request)
	uibpv2ts.True(uibpv2ts.env.IsWorkflowCompleted())
	uibpv2ts.Error(uibpv2ts.env.GetWorkflowError())
}

func TestUpdateIBPartitionV2TestSuite(t *testing.T) {
	suite.Run(t, new(UpdateIBPartitionV2TestSuite))
}
