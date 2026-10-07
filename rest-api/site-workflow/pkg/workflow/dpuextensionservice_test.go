// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	"google.golang.org/protobuf/types/known/timestamppb"
)

type InventoryDpuExtensionServiceTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *InventoryDpuExtensionServiceTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *InventoryDpuExtensionServiceTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *InventoryDpuExtensionServiceTestSuite) Test_DiscoverDpuExtensionServiceInventory_Success() {
	var inventoryManager iActivity.ManageDpuExtensionServiceInventory

	s.env.RegisterActivity(inventoryManager.DiscoverDpuExtensionServiceInventory)
	s.env.OnActivity(inventoryManager.DiscoverDpuExtensionServiceInventory, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DiscoverDpuExtensionServiceInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *InventoryDpuExtensionServiceTestSuite) Test_DiscoverDpuExtensionServiceInventory_ActivityFails() {
	var inventoryManager iActivity.ManageDpuExtensionServiceInventory

	errMsg := "Site Controller communication error"

	s.env.RegisterActivity(inventoryManager.DiscoverDpuExtensionServiceInventory)
	s.env.OnActivity(inventoryManager.DiscoverDpuExtensionServiceInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	s.env.ExecuteWorkflow(DiscoverDpuExtensionServiceInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestInventoryDpuExtensionServiceTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryDpuExtensionServiceTestSuite))
}

type CreateDpuExtensionServiceTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *CreateDpuExtensionServiceTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *CreateDpuExtensionServiceTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *CreateDpuExtensionServiceTestSuite) Test_CreateDpuExtensionService_Success() {
	var dpuExtensionServiceManager iActivity.ManageDpuExtensionService

	request := &corev1.CreateDpuExtensionServiceRequest{
		ServiceId:            util.GetStrPtr("test-service-id"),
		ServiceName:          "test-service-name",
		ServiceType:          corev1.DpuExtensionServiceType_KUBERNETES_POD,
		TenantOrganizationId: "test-tenant-org-id",
	}

	expectedResult := &corev1.DpuExtensionService{
		ServiceId:            *request.ServiceId,
		ServiceName:          request.ServiceName,
		ServiceType:          request.ServiceType,
		TenantOrganizationId: request.TenantOrganizationId,
	}

	// Mock CreateDpuExtensionServiceOnSite activity
	s.env.RegisterActivity(dpuExtensionServiceManager.CreateDpuExtensionServiceOnSite)
	s.env.OnActivity(dpuExtensionServiceManager.CreateDpuExtensionServiceOnSite, mock.Anything, mock.Anything).Return(expectedResult, nil)

	// Execute CreateDpuExtensionService workflow
	s.env.ExecuteWorkflow(CreateDpuExtensionService, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())

	var result *corev1.DpuExtensionService
	s.env.GetWorkflowResult(&result)

	s.Equal(result.ServiceId, *request.ServiceId)
	s.Equal(result.ServiceName, request.ServiceName)
	s.Equal(result.ServiceType, request.ServiceType)
	s.Equal(result.TenantOrganizationId, request.TenantOrganizationId)
}

func (s *CreateDpuExtensionServiceTestSuite) Test_CreateDpuExtensionService_Failure() {
	var dpuExtensionServiceManager iActivity.ManageDpuExtensionService

	request := &corev1.CreateDpuExtensionServiceRequest{
		ServiceId:            util.GetStrPtr("test-service-id"),
		ServiceName:          "test-service-name",
		TenantOrganizationId: "test-tenant-org-id",
	}

	errMsg := "Site Controller communication error"

	// Mock CreateDpuExtensionServiceOnSite activity
	s.env.RegisterActivity(dpuExtensionServiceManager.CreateDpuExtensionServiceOnSite)
	s.env.OnActivity(dpuExtensionServiceManager.CreateDpuExtensionServiceOnSite, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// execute CreateDpuExtensionService workflow
	s.env.ExecuteWorkflow(CreateDpuExtensionService, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestCreateDpuExtensionServiceTestSuite(t *testing.T) {
	suite.Run(t, new(CreateDpuExtensionServiceTestSuite))
}

type UpdateDpuExtensionServiceTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateDpuExtensionServiceTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateDpuExtensionServiceTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateDpuExtensionServiceTestSuite) Test_UpdateDpuExtensionService_Success() {
	var dpuExtensionServiceManager iActivity.ManageDpuExtensionService

	request := &corev1.UpdateDpuExtensionServiceRequest{
		ServiceId:   "test-service-id",
		ServiceName: util.GetStrPtr("updated-service-name"),
		Description: util.GetStrPtr("updated-service-description"),
	}

	expectedResult := &corev1.DpuExtensionService{
		ServiceId:   request.ServiceId,
		ServiceName: *request.ServiceName,
		Description: *request.Description,
	}

	// Mock UpdateDpuExtensionServiceOnSite activity
	s.env.RegisterActivity(dpuExtensionServiceManager.UpdateDpuExtensionServiceOnSite)
	s.env.OnActivity(dpuExtensionServiceManager.UpdateDpuExtensionServiceOnSite, mock.Anything, mock.Anything).Return(expectedResult, nil)

	// Execute UpdateDpuExtensionService workflow
	s.env.ExecuteWorkflow(UpdateDpuExtensionService, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())

	var result *corev1.DpuExtensionService
	s.env.GetWorkflowResult(&result)

	s.Equal(result.ServiceId, request.ServiceId)
	s.Equal(result.ServiceName, *request.ServiceName)
	s.Equal(result.Description, *request.Description)
}

func (s *UpdateDpuExtensionServiceTestSuite) Test_UpdateDpuExtensionService_Failure() {
	var dpuExtensionServiceManager iActivity.ManageDpuExtensionService

	request := &corev1.UpdateDpuExtensionServiceRequest{
		ServiceId:   "test-service-id",
		ServiceName: util.GetStrPtr("updated-service-name"),
		Description: util.GetStrPtr("updated-service-description"),
	}

	errMsg := "Site Controller communication error"

	// Mock UpdateDpuExtensionServiceOnSite activity
	s.env.RegisterActivity(dpuExtensionServiceManager.UpdateDpuExtensionServiceOnSite)
	s.env.OnActivity(dpuExtensionServiceManager.UpdateDpuExtensionServiceOnSite, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// execute UpdateDpuExtensionService workflow
	s.env.ExecuteWorkflow(UpdateDpuExtensionService, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestUpdateDpuExtensionServiceTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateDpuExtensionServiceTestSuite))
}

type DeleteDpuExtensionServiceTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *DeleteDpuExtensionServiceTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *DeleteDpuExtensionServiceTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *DeleteDpuExtensionServiceTestSuite) Test_DeleteDpuExtensionService_Success() {
	var dpuExtensionServiceManager iActivity.ManageDpuExtensionService

	request := &corev1.DeleteDpuExtensionServiceRequest{
		ServiceId: "test-service-id",
	}

	// Mock DeleteDpuExtensionServiceOnSite activity
	s.env.RegisterActivity(dpuExtensionServiceManager.DeleteDpuExtensionServiceOnSite)
	s.env.OnActivity(dpuExtensionServiceManager.DeleteDpuExtensionServiceOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DeleteDpuExtensionService, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *DeleteDpuExtensionServiceTestSuite) Test_DeleteDpuExtensionService_Failure() {
	var dpuExtensionServiceManager iActivity.ManageDpuExtensionService

	request := &corev1.DeleteDpuExtensionServiceRequest{
		ServiceId: "test-service-id",
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteDpuExtensionServiceOnSite activity
	s.env.RegisterActivity(dpuExtensionServiceManager.DeleteDpuExtensionServiceOnSite)
	s.env.OnActivity(dpuExtensionServiceManager.DeleteDpuExtensionServiceOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteDpuExtensionService workflow
	s.env.ExecuteWorkflow(DeleteDpuExtensionService, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestDeleteDpuExtensionServiceTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteDpuExtensionServiceTestSuite))
}

type GetDpuExtensionServiceVersionsInfoTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *GetDpuExtensionServiceVersionsInfoTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *GetDpuExtensionServiceVersionsInfoTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *GetDpuExtensionServiceVersionsInfoTestSuite) Test_GetDpuExtensionServiceVersionsInfo_Success() {
	var dpuExtensionServiceManager iActivity.ManageDpuExtensionService

	request := &corev1.GetDpuExtensionServiceVersionsInfoRequest{
		ServiceId: "test-service-id",
	}

	versionInfo := &corev1.DpuExtensionServiceVersionInfo{
		Version:       "V1-T1234567890",
		Data:          "test data",
		HasCredential: false,
		Created:       timestamppb.New(time.Now()).String(),
	}

	// Mock GetDpuExtensionServiceVersionsInfoOnSite activity
	s.env.RegisterActivity(dpuExtensionServiceManager.GetDpuExtensionServiceVersionsInfoOnSite)
	s.env.OnActivity(dpuExtensionServiceManager.GetDpuExtensionServiceVersionsInfoOnSite, mock.Anything, mock.Anything).Return(&corev1.DpuExtensionServiceVersionInfoList{
		VersionInfos: []*corev1.DpuExtensionServiceVersionInfo{
			versionInfo,
		},
	}, nil)

	// execute workflow
	s.env.ExecuteWorkflow(GetDpuExtensionServiceVersionsInfo, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())

	var result *corev1.DpuExtensionServiceVersionInfoList
	s.env.GetWorkflowResult(&result)

	s.Equal(1, len(result.VersionInfos))
	s.Equal(versionInfo.Version, result.VersionInfos[0].Version)
	s.Equal(versionInfo.Data, result.VersionInfos[0].Data)
	s.Equal(versionInfo.HasCredential, result.VersionInfos[0].HasCredential)
	s.Equal(versionInfo.Created, result.VersionInfos[0].Created)
}

func (s *GetDpuExtensionServiceVersionsInfoTestSuite) Test_GetDpuExtensionServiceVersionsInfo_Failure() {
	var dpuExtensionServiceManager iActivity.ManageDpuExtensionService

	request := &corev1.GetDpuExtensionServiceVersionsInfoRequest{
		ServiceId: "test-service-id",
	}

	errMsg := "Site Controller communication error"

	// Mock GetDpuExtensionServiceVersionsInfoOnSite activity
	s.env.RegisterActivity(dpuExtensionServiceManager.GetDpuExtensionServiceVersionsInfoOnSite)
	s.env.OnActivity(dpuExtensionServiceManager.GetDpuExtensionServiceVersionsInfoOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute GetDpuExtensionServiceVersionsInfo workflow
	s.env.ExecuteWorkflow(GetDpuExtensionServiceVersionsInfo, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestGetDpuExtensionServiceVersionsInfoTestSuite(t *testing.T) {
	suite.Run(t, new(GetDpuExtensionServiceVersionsInfoTestSuite))
}
