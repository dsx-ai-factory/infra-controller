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
	"go.temporal.io/sdk/testsuite"
)

type CreateOsImageTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ctts *CreateOsImageTestSuite) SetupTest() {
	ctts.env = ctts.NewTestWorkflowEnvironment()
}

func (ctts *CreateOsImageTestSuite) AfterTest(suiteName, testName string) {
	ctts.env.AssertExpectations(ctts.T())
}

func (ctts *CreateOsImageTestSuite) Test_CreateOsImage_Success() {
	var operatingSystemManager iActivity.ManageOperatingSystem

	request := &corev1.OsImageAttributes{
		TenantOrganizationId: "m4jjok8wsg",
	}

	// Mock CreateOsImageOnSite activity
	ctts.env.RegisterActivity(operatingSystemManager.CreateOsImageOnSite)
	ctts.env.OnActivity(operatingSystemManager.CreateOsImageOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute CreateOsImage workflow
	ctts.env.ExecuteWorkflow(CreateOsImage, request)
	ctts.True(ctts.env.IsWorkflowCompleted())
	ctts.NoError(ctts.env.GetWorkflowError())
}

func (ctts *CreateOsImageTestSuite) Test_CreateOsImage_Failure() {
	var operatingSystemManager iActivity.ManageOperatingSystem

	request := &corev1.OsImageAttributes{
		TenantOrganizationId: "m4jjok8wsg",
	}

	errMsg := "Site Controller communication error"

	// Mock CreateOsImageOnSite activity
	ctts.env.RegisterActivity(operatingSystemManager.CreateOsImageOnSite)
	ctts.env.OnActivity(operatingSystemManager.CreateOsImageOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// Execute CreateOsImage workflow
	ctts.env.ExecuteWorkflow(CreateOsImage, request)
	ctts.True(ctts.env.IsWorkflowCompleted())
	ctts.Error(ctts.env.GetWorkflowError())
}

func TestCreateOsImageTestSuite(t *testing.T) {
	suite.Run(t, new(CreateOsImageTestSuite))
}

type UpdateOsImageTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateOsImageTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateOsImageTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateOsImageTestSuite) Test_UpdateOsImage_Success() {
	var operatingSystemManager iActivity.ManageOperatingSystem

	request := &corev1.OsImageAttributes{
		TenantOrganizationId: "m4jjok8wsg",
		Name:                 util.GetStrPtr("updated-os-name"),
	}

	// Mock UpdateOsImageOnSite activity
	s.env.RegisterActivity(operatingSystemManager.UpdateOsImageOnSite)
	s.env.OnActivity(operatingSystemManager.UpdateOsImageOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(UpdateOsImage, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateOsImageTestSuite) Test_UpdateOsImage_Failure() {
	var operatingSystemManager iActivity.ManageOperatingSystem

	request := &corev1.OsImageAttributes{
		TenantOrganizationId: "m4jjok8wsg",
		Name:                 util.GetStrPtr("updated-os-name"),
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateOsImageOnSite activity
	s.env.RegisterActivity(operatingSystemManager.UpdateOsImageOnSite)
	s.env.OnActivity(operatingSystemManager.UpdateOsImageOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute UpdateOsImage workflow
	s.env.ExecuteWorkflow(UpdateOsImage, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestUpdateOsImageTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateOsImageTestSuite))
}

type DeleteOsImageTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ctts *DeleteOsImageTestSuite) SetupTest() {
	ctts.env = ctts.NewTestWorkflowEnvironment()
}

func (ctts *DeleteOsImageTestSuite) AfterTest(suiteName, testName string) {
	ctts.env.AssertExpectations(ctts.T())
}

func (ctts *DeleteOsImageTestSuite) Test_DeleteOsImage_Success() {
	var operatingSystemManager iActivity.ManageOperatingSystem

	request := &corev1.DeleteOsImageRequest{
		Id:                   &corev1.UUID{Value: uuid.New().String()},
		TenantOrganizationId: "m4jjok8wsg",
	}

	// Mock CreateOsImageOnSite activity
	ctts.env.RegisterActivity(operatingSystemManager.DeleteOsImageOnSite)
	ctts.env.OnActivity(operatingSystemManager.DeleteOsImageOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute CreateOsImage workflow
	ctts.env.ExecuteWorkflow(DeleteOsImage, request)
	ctts.True(ctts.env.IsWorkflowCompleted())
	ctts.NoError(ctts.env.GetWorkflowError())
}

func (ctts *DeleteOsImageTestSuite) Test_DeleteOsImage_Failure() {
	var operatingSystemManager iActivity.ManageOperatingSystem

	request := &corev1.DeleteOsImageRequest{
		Id:                   &corev1.UUID{Value: uuid.New().String()},
		TenantOrganizationId: "m4jjok8wsg",
	}

	errMsg := "Site Controller communication error"

	// Mock CreateOsImageOnSite activity
	ctts.env.RegisterActivity(operatingSystemManager.DeleteOsImageOnSite)
	ctts.env.OnActivity(operatingSystemManager.DeleteOsImageOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// Execute CreateOsImage workflow
	ctts.env.ExecuteWorkflow(DeleteOsImage, request)
	ctts.True(ctts.env.IsWorkflowCompleted())
	ctts.Error(ctts.env.GetWorkflowError())
}

func TestDeleteOsImageTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteOsImageTestSuite))
}
