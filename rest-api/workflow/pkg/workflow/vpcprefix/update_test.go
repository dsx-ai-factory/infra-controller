// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcprefix

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	vpcPrefixActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/vpcprefix"
)

type UpdateVpcPrefixTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateVpcPrefixTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateVpcPrefixTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateVpcPrefixTestSuite) Test_UpdateVpcPrefixInventory_Success() {

	var VpcPrefixManager vpcPrefixActivity.ManageVpcPrefix

	siteID := uuid.New()
	VpcPrefixInventory := &corev1.VpcPrefixInventory{
		VpcPrefixes: []*corev1.VpcPrefix{
			{
				Id: &corev1.VpcPrefixId{Value: uuid.NewString()},
			},
			{
				Id: &corev1.VpcPrefixId{Value: uuid.NewString()},
			},
		},
	}

	// Mock UpdateVpcPrefixViaSiteAgent activity
	s.env.RegisterActivity(VpcPrefixManager.UpdateVpcPrefixesInDB)
	s.env.OnActivity(VpcPrefixManager.UpdateVpcPrefixesInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// execute UpdateVpcPrefixInventory workflow
	s.env.ExecuteWorkflow(UpdateVpcPrefixInventory, siteID.String(), VpcPrefixInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateVpcPrefixTestSuite) Test_UpdateVpcPrefixInventory_ActivityFails() {

	var VpcPrefixManager vpcPrefixActivity.ManageVpcPrefix

	siteID := uuid.New()
	VpcPrefixInventory := &corev1.VpcPrefixInventory{
		VpcPrefixes: []*corev1.VpcPrefix{
			{
				Id: &corev1.VpcPrefixId{Value: uuid.NewString()},
			},
			{
				Id: &corev1.VpcPrefixId{Value: uuid.NewString()},
			},
		},
	}

	// Mock UpdateVpcPrefixesInDB activity failure
	s.env.RegisterActivity(VpcPrefixManager.UpdateVpcPrefixesInDB)
	s.env.OnActivity(VpcPrefixManager.UpdateVpcPrefixesInDB, mock.Anything, mock.Anything, mock.Anything).Return(errors.New("UpdateVpcPrefixInventory Failure"))

	// execute UpdateVpcPrefixStatus workflow
	s.env.ExecuteWorkflow(UpdateVpcPrefixInventory, siteID.String(), VpcPrefixInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateVpcPrefixInventory Failure", applicationErr.Error())
}

func TestUpdateVpcPrefixSuite(t *testing.T) {
	suite.Run(t, new(UpdateVpcPrefixTestSuite))
}
