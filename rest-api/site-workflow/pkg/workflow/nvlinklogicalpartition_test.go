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

type InventoryNVLinkLogicalPartitionTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *InventoryNVLinkLogicalPartitionTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *InventoryNVLinkLogicalPartitionTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *InventoryNVLinkLogicalPartitionTestSuite) Test_DiscoverNVLinkLogicalPartitionInventory_Success() {
	var inventoryManager iActivity.ManageNVLinkLogicalPartitionInventory

	s.env.RegisterActivity(inventoryManager.DiscoverNVLinkLogicalPartitionInventory)
	s.env.OnActivity(inventoryManager.DiscoverNVLinkLogicalPartitionInventory, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DiscoverNVLinkLogicalPartitionInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *InventoryNVLinkLogicalPartitionTestSuite) Test_DiscoverNVLinkLogicalPartitionInventory_ActivityFails() {
	var inventoryManager iActivity.ManageNVLinkLogicalPartitionInventory

	errMsg := "Site Controller communication error"

	s.env.RegisterActivity(inventoryManager.DiscoverNVLinkLogicalPartitionInventory)
	s.env.OnActivity(inventoryManager.DiscoverNVLinkLogicalPartitionInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	s.env.ExecuteWorkflow(DiscoverNVLinkLogicalPartitionInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestInventoryNVLinkLogicalPartitionTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryNVLinkLogicalPartitionTestSuite))
}

type CreateNVLinkLogicalPartitionTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cnvllvts *CreateNVLinkLogicalPartitionTestSuite) SetupTest() {
	cnvllvts.env = cnvllvts.NewTestWorkflowEnvironment()
}

func (cnvllvts *CreateNVLinkLogicalPartitionTestSuite) AfterTest(suiteName, testName string) {
	cnvllvts.env.AssertExpectations(cnvllvts.T())
}

func (cnvllvts *CreateNVLinkLogicalPartitionTestSuite) Test_CreateNVLinkLogicalPartition_Success() {
	var NVLinkLogicalPartitionManager iActivity.ManageNVLinkLogicalPartition

	request := &corev1.NVLinkLogicalPartitionCreationRequest{
		Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Config: &corev1.NVLinkLogicalPartitionConfig{
			Metadata: &corev1.Metadata{
				Name: "the_name",
			},
			TenantOrganizationId: "test_org",
		},
	}

	nvLinkLogicalPartition := &corev1.NVLinkLogicalPartition{
		Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Config: &corev1.NVLinkLogicalPartitionConfig{
			Metadata: &corev1.Metadata{
				Name: "the_name",
			},
			TenantOrganizationId: "test_org",
		},
	}

	// Mock CreateNVLinkLogicalPartitionOnSite activity
	cnvllvts.env.RegisterActivity(NVLinkLogicalPartitionManager.CreateNVLinkLogicalPartitionOnSite)
	cnvllvts.env.OnActivity(NVLinkLogicalPartitionManager.CreateNVLinkLogicalPartitionOnSite, mock.Anything, mock.Anything).Return(nvLinkLogicalPartition, nil)

	// Execute CreateNVLinkLogicalPartition workflow
	cnvllvts.env.ExecuteWorkflow(CreateNVLinkLogicalPartition, request)
	cnvllvts.True(cnvllvts.env.IsWorkflowCompleted())
	cnvllvts.NoError(cnvllvts.env.GetWorkflowError())

	// Verify result
	var result corev1.NVLinkLogicalPartition
	cnvllvts.NoError(cnvllvts.env.GetWorkflowResult(&result))
	cnvllvts.NotNil(result.Id)
	cnvllvts.Equal(nvLinkLogicalPartition.Id.Value, result.Id.Value)
	cnvllvts.NotNil(result.Config)
	cnvllvts.Equal(nvLinkLogicalPartition.Config.Metadata.Name, result.Config.Metadata.Name)
	cnvllvts.Equal(nvLinkLogicalPartition.Config.TenantOrganizationId, result.Config.TenantOrganizationId)
}

func (cnvllvts *CreateNVLinkLogicalPartitionTestSuite) Test_CreateNVLinkLogicalPartition_Failure() {
	var NVLinkLogicalPartitionManager iActivity.ManageNVLinkLogicalPartition

	request := &corev1.NVLinkLogicalPartitionCreationRequest{
		Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Config: &corev1.NVLinkLogicalPartitionConfig{
			Metadata: &corev1.Metadata{
				Name: "the_name",
			},
			TenantOrganizationId: "test_org",
		},
	}

	errMsg := "Site Controller communication error"

	// Mock CreateNVLinkLogicalPartitionOnSite activity
	cnvllvts.env.RegisterActivity(NVLinkLogicalPartitionManager.CreateNVLinkLogicalPartitionOnSite)
	cnvllvts.env.OnActivity(NVLinkLogicalPartitionManager.CreateNVLinkLogicalPartitionOnSite, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// execute CreateNVLinkLogicalPartition workflow
	cnvllvts.env.ExecuteWorkflow(CreateNVLinkLogicalPartition, request)
	cnvllvts.True(cnvllvts.env.IsWorkflowCompleted())
	cnvllvts.Error(cnvllvts.env.GetWorkflowError())
}

func TestCreateNVLinkLogicalPartitionTestSuite(t *testing.T) {
	suite.Run(t, new(CreateNVLinkLogicalPartitionTestSuite))
}

type UpdateNVLinkLogicalPartitionTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cnvllvts *UpdateNVLinkLogicalPartitionTestSuite) SetupTest() {
	cnvllvts.env = cnvllvts.NewTestWorkflowEnvironment()
}

func (cnvllvts *UpdateNVLinkLogicalPartitionTestSuite) AfterTest(suiteName, testName string) {
	cnvllvts.env.AssertExpectations(cnvllvts.T())
}

func (cnvllvts *UpdateNVLinkLogicalPartitionTestSuite) Test_UpdateNVLinkLogicalPartition_Success() {
	var NVLinkLogicalPartitionManager iActivity.ManageNVLinkLogicalPartition

	request := &corev1.NVLinkLogicalPartitionUpdateRequest{
		Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Config: &corev1.NVLinkLogicalPartitionConfig{
			Metadata: &corev1.Metadata{
				Name: "the_name",
			},
		},
	}

	// Mock UpdateNVLinkLogicalPartitionOnSite activity
	cnvllvts.env.RegisterActivity(NVLinkLogicalPartitionManager.UpdateNVLinkLogicalPartitionOnSite)
	cnvllvts.env.OnActivity(NVLinkLogicalPartitionManager.UpdateNVLinkLogicalPartitionOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute UpdateNVLinkLogicalPartition workflow
	cnvllvts.env.ExecuteWorkflow(UpdateNVLinkLogicalPartition, request)
	cnvllvts.True(cnvllvts.env.IsWorkflowCompleted())
	cnvllvts.NoError(cnvllvts.env.GetWorkflowError())
}

func (cnvllvts *UpdateNVLinkLogicalPartitionTestSuite) Test_UpdateNVLinkLogicalPartition_Failure() {
	var NVLinkLogicalPartitionManager iActivity.ManageNVLinkLogicalPartition

	request := &corev1.NVLinkLogicalPartitionUpdateRequest{
		Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
		Config: &corev1.NVLinkLogicalPartitionConfig{
			Metadata: &corev1.Metadata{
				Name: "the_name",
			},
		},
	}

	errMsg := "Site Controller communication error"

	// Mock CreateNVLinkLogicalPartitionOnSite activity
	cnvllvts.env.RegisterActivity(NVLinkLogicalPartitionManager.UpdateNVLinkLogicalPartitionOnSite)
	cnvllvts.env.OnActivity(NVLinkLogicalPartitionManager.UpdateNVLinkLogicalPartitionOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateNVLinkLogicalPartition workflow
	cnvllvts.env.ExecuteWorkflow(UpdateNVLinkLogicalPartition, request)
	cnvllvts.True(cnvllvts.env.IsWorkflowCompleted())
	cnvllvts.Error(cnvllvts.env.GetWorkflowError())
}

func TestUpdateNVLinkLogicalPartitionTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateNVLinkLogicalPartitionTestSuite))
}

type DeleteNVLinkLogicalPartitionTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (cnvllvts *DeleteNVLinkLogicalPartitionTestSuite) SetupTest() {
	cnvllvts.env = cnvllvts.NewTestWorkflowEnvironment()
}

func (cnvllvts *DeleteNVLinkLogicalPartitionTestSuite) AfterTest(suiteName, testName string) {
	cnvllvts.env.AssertExpectations(cnvllvts.T())
}

func (cnvllvts *DeleteNVLinkLogicalPartitionTestSuite) Test_DeleteNVLinkLogicalPartition_Success() {
	var NVLinkLogicalPartitionManager iActivity.ManageNVLinkLogicalPartition

	request := &corev1.NVLinkLogicalPartitionDeletionRequest{
		Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
	}

	// Mock DeleteNVLinkLogicalPartitionOnSite activity
	cnvllvts.env.RegisterActivity(NVLinkLogicalPartitionManager.DeleteNVLinkLogicalPartitionOnSite)
	cnvllvts.env.OnActivity(NVLinkLogicalPartitionManager.DeleteNVLinkLogicalPartitionOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	cnvllvts.env.ExecuteWorkflow(DeleteNVLinkLogicalPartition, request)
	cnvllvts.True(cnvllvts.env.IsWorkflowCompleted())
	cnvllvts.NoError(cnvllvts.env.GetWorkflowError())
}

func (cnvllvts *DeleteNVLinkLogicalPartitionTestSuite) Test_DeleteNVLinkLogicalPartition_Failure() {
	var NVLinkLogicalPartitionManager iActivity.ManageNVLinkLogicalPartition

	request := &corev1.NVLinkLogicalPartitionDeletionRequest{
		Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteNVLinkLogicalPartitionOnSite activity
	cnvllvts.env.RegisterActivity(NVLinkLogicalPartitionManager.DeleteNVLinkLogicalPartitionOnSite)
	cnvllvts.env.OnActivity(NVLinkLogicalPartitionManager.DeleteNVLinkLogicalPartitionOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteNVLinkLogicalPartitionV2 workflow
	cnvllvts.env.ExecuteWorkflow(DeleteNVLinkLogicalPartition, request)
	cnvllvts.True(cnvllvts.env.IsWorkflowCompleted())
	cnvllvts.Error(cnvllvts.env.GetWorkflowError())
}

func TestDeleteNVLinkLogicalPartitionTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteNVLinkLogicalPartitionTestSuite))
}
