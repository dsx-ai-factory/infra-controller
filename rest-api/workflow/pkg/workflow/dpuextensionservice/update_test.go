// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dpuextensionservice

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/dpuextensionservice"
)

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

func (s *UpdateDpuExtensionServiceTestSuite) Test_UpdateDpuExtensionServiceInventory_Success() {
	var dpuExtensionServiceManager dpuextensionservice.ManageDpuExtensionService

	siteID := uuid.New()
	DpuExtensionServiceInventory := &corev1.DpuExtensionServiceInventory{
		DpuExtensionServices: []*corev1.DpuExtensionService{
			{ServiceId: uuid.NewString()},
			{ServiceId: uuid.NewString()},
		},
	}

	// Mock UpdateDpuExtensionServicesInDB activity
	s.env.RegisterActivity(dpuExtensionServiceManager.UpdateDpuExtensionServicesInDB)
	s.env.OnActivity(dpuExtensionServiceManager.UpdateDpuExtensionServicesInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateDpuExtensionServiceInventory workflow
	s.env.ExecuteWorkflow(UpdateDpuExtensionServiceInventory, siteID.String(), DpuExtensionServiceInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateDpuExtensionServiceTestSuite) Test_UpdateDpuExtensionServiceInventory_ActivityFails() {
	var dpuExtensionServiceManager dpuextensionservice.ManageDpuExtensionService

	siteID := uuid.New()
	DpuExtensionServiceInventory := &corev1.DpuExtensionServiceInventory{
		DpuExtensionServices: []*corev1.DpuExtensionService{
			{ServiceId: uuid.NewString()},
			{ServiceId: uuid.NewString()},
		},
	}

	// Mock UpdateDpuExtensionServicesInDB activity failure
	s.env.RegisterActivity(dpuExtensionServiceManager.UpdateDpuExtensionServicesInDB)
	s.env.OnActivity(dpuExtensionServiceManager.UpdateDpuExtensionServicesInDB, mock.Anything, mock.Anything, mock.Anything).Return(errors.New("UpdateDpuExtensionServicesInDB Failure"))

	// execute UpdateDpuExtensionServiceInventory workflow
	s.env.ExecuteWorkflow(UpdateDpuExtensionServiceInventory, siteID.String(), DpuExtensionServiceInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateDpuExtensionServicesInDB Failure", applicationErr.Error())
}

func TestUpdateDpuExtensionServiceSuite(t *testing.T) {
	suite.Run(t, new(UpdateDpuExtensionServiceTestSuite))
}
