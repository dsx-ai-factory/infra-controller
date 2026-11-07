// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instance

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	instanceActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
)

type RebootInstanceTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *RebootInstanceTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *RebootInstanceTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *RebootInstanceTestSuite) Test_RebootInstanceByIDWorkflow_Success() {
	var instanceManager instanceActivity.ManageInstance

	instanceID := uuid.New()
	request := &corev1.InstancePowerRequest{
		InstanceId: &corev1.InstanceId{
			Value: instanceID.String(),
		},
		BootWithCustomIpxe:   true,
		ApplyUpdatesOnReboot: true,
	}

	// Mock RebootInstanceOnSite activity
	s.env.RegisterActivity(instanceManager.RebootInstanceOnSite)
	s.env.OnActivity(instanceManager.RebootInstanceOnSite, mock.Anything, request).Return(nil)

	// execute RebootInstanceByID workflow
	s.env.ExecuteWorkflow(RebootInstanceByID, instanceID, true, true)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *RebootInstanceTestSuite) Test_RebootInstanceByIDWorkflow_ActivityFailsErrorActivityFails() {
	var instanceManager instanceActivity.ManageInstance

	instanceID := uuid.New()
	request := &corev1.InstancePowerRequest{
		InstanceId: &corev1.InstanceId{
			Value: instanceID.String(),
		},
		BootWithCustomIpxe:   true,
		ApplyUpdatesOnReboot: true,
	}

	// Mock RebootInstanceViaSiteAgent activity failure
	s.env.RegisterActivity(instanceManager.RebootInstanceOnSite)
	s.env.OnActivity(instanceManager.RebootInstanceOnSite, mock.Anything, request).Return(errors.New("RebootInstanceOnSite Failure"))

	// execute RebootInstanceByID workflow
	s.env.ExecuteWorkflow(RebootInstanceByID, instanceID, true, true)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("RebootInstanceOnSite Failure", applicationErr.Error())
}

func TestRebootInstanceSuite(t *testing.T) {
	suite.Run(t, new(RebootInstanceTestSuite))
}
