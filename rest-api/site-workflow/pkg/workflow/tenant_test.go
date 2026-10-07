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

type CreateTenantTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ctts *CreateTenantTestSuite) SetupTest() {
	ctts.env = ctts.NewTestWorkflowEnvironment()
}

func (ctts *CreateTenantTestSuite) AfterTest(suiteName, testName string) {
	ctts.env.AssertExpectations(ctts.T())
}

func (ctts *CreateTenantTestSuite) Test_CreateTenant_Success() {
	var tenantManager iActivity.ManageTenant

	request := &corev1.CreateTenantRequest{
		OrganizationId: "m4jjok8wsg",
	}

	// Mock CreateTenantOnSite activity
	ctts.env.RegisterActivity(tenantManager.CreateTenantOnSite)
	ctts.env.OnActivity(tenantManager.CreateTenantOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute CreateTenant workflow
	ctts.env.ExecuteWorkflow(CreateTenant, request)
	ctts.True(ctts.env.IsWorkflowCompleted())
	ctts.NoError(ctts.env.GetWorkflowError())
}

func (ctts *CreateTenantTestSuite) Test_CreateTenant_Failure() {
	var tenantManager iActivity.ManageTenant

	request := &corev1.CreateTenantRequest{
		OrganizationId: "m4jjok8wsg",
	}

	errMsg := "Site Controller communication error"

	// Mock CreateTenantOnSite activity
	ctts.env.RegisterActivity(tenantManager.CreateTenantOnSite)
	ctts.env.OnActivity(tenantManager.CreateTenantOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// Execute CreateTenant workflow
	ctts.env.ExecuteWorkflow(CreateTenant, request)
	ctts.True(ctts.env.IsWorkflowCompleted())
	ctts.Error(ctts.env.GetWorkflowError())
}

func TestCreateTenantTestSuite(t *testing.T) {
	suite.Run(t, new(CreateTenantTestSuite))
}

type UpdateTenantTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateTenantTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateTenantTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateTenantTestSuite) Test_UpdateTenant_Success() {
	var tenantManager iActivity.ManageTenant

	request := &corev1.UpdateTenantRequest{
		OrganizationId: "m4jjok8wsg",
		Metadata: &corev1.Metadata{
			Name: "updated-tenant-name",
		},
	}

	// Mock UpdateTenantOnSite activity
	s.env.RegisterActivity(tenantManager.UpdateTenantOnSite)
	s.env.OnActivity(tenantManager.UpdateTenantOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(UpdateTenant, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateTenantTestSuite) Test_UpdateTenant_Failure() {
	var tenantManager iActivity.ManageTenant

	request := &corev1.UpdateTenantRequest{
		OrganizationId: "m4jjok8wsg",
		Metadata: &corev1.Metadata{
			Name: "updated-tenant-name",
		},
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateTenantOnSite activity
	s.env.RegisterActivity(tenantManager.UpdateTenantOnSite)
	s.env.OnActivity(tenantManager.UpdateTenantOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateTenant workflow
	s.env.ExecuteWorkflow(UpdateTenant, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestUpdateTenantTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateTenantTestSuite))
}

type DiscoverTenantInventoryTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ivts *DiscoverTenantInventoryTestSuite) SetupTest() {
	ivts.env = ivts.NewTestWorkflowEnvironment()
}

func (ivts *DiscoverTenantInventoryTestSuite) AfterTest(suiteName, testName string) {
	ivts.env.AssertExpectations(ivts.T())
}

func (ivts *DiscoverTenantInventoryTestSuite) Test_DiscoverTenantInventory_Success() {
	var inventoryManager iActivity.ManageTenantInventory

	ivts.env.RegisterActivity(inventoryManager.DiscoverTenantInventory)
	ivts.env.OnActivity(inventoryManager.DiscoverTenantInventory, mock.Anything).Return(nil)

	// execute workflow
	ivts.env.ExecuteWorkflow(DiscoverTenantInventory)
	ivts.True(ivts.env.IsWorkflowCompleted())
	ivts.NoError(ivts.env.GetWorkflowError())
}

func (ivts *DiscoverTenantInventoryTestSuite) Test_DiscoverTenantInventory_ActivityFails() {
	var inventoryManager iActivity.ManageTenantInventory

	errMsg := "Site Controller communication error"

	ivts.env.RegisterActivity(inventoryManager.DiscoverTenantInventory)
	ivts.env.OnActivity(inventoryManager.DiscoverTenantInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	ivts.env.ExecuteWorkflow(DiscoverTenantInventory)
	ivts.True(ivts.env.IsWorkflowCompleted())
	err := ivts.env.GetWorkflowError()
	ivts.Error(err)

	var applicationErr *temporal.ApplicationError
	ivts.True(errors.As(err, &applicationErr))
	ivts.Equal(errMsg, applicationErr.Error())
}

func TestDiscoverTenantInventoryTestSuite(t *testing.T) {
	suite.Run(t, new(DiscoverTenantInventoryTestSuite))
}
