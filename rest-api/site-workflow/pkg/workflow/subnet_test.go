// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"

	"go.temporal.io/sdk/temporal"

	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
)

type CreateSubnetV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *CreateSubnetV2TestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *CreateSubnetV2TestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *CreateSubnetV2TestSuite) Test_CreateSubnetV2_Success() {
	var SubnetManager iActivity.ManageSubnet

	subnetName := "the_best_subnet"
	vpcID := "9001"

	request := &corev1.NetworkSegmentCreationRequest{
		Name:  subnetName,
		VpcId: &corev1.VpcId{Value: vpcID},
		Prefixes: []*corev1.NetworkPrefix{
			{
				Prefix: "10.0.0.1/8",
			},
		},
	}

	// Mock CreateSubnetOnSite activity
	s.env.RegisterActivity(SubnetManager.CreateSubnetOnSite)
	s.env.OnActivity(SubnetManager.CreateSubnetOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(CreateSubnetV2, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *CreateSubnetV2TestSuite) Test_CreateSubnetV2_Failure() {
	var SubnetManager iActivity.ManageSubnet

	subnetName := "the_best_subnet"
	vpcID := "9001"

	request := &corev1.NetworkSegmentCreationRequest{
		Name:  subnetName,
		VpcId: &corev1.VpcId{Value: vpcID},
		Prefixes: []*corev1.NetworkPrefix{
			{
				Prefix: "10.0.0.1/8",
			},
		},
	}

	errMsg := "Site Controller communication error"

	// Mock CreateSubnetOnSite activity
	s.env.RegisterActivity(SubnetManager.CreateSubnetOnSite)
	s.env.OnActivity(SubnetManager.CreateSubnetOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute CreateMachineInventory workflow
	s.env.ExecuteWorkflow(CreateSubnetV2, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestCreateSubnetV2TestSuite(t *testing.T) {
	suite.Run(t, new(CreateSubnetV2TestSuite))
}

type DeleteSubnetV2TestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *DeleteSubnetV2TestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *DeleteSubnetV2TestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *DeleteSubnetV2TestSuite) Test_DeleteSubnetV2_Success() {
	var SubnetManager iActivity.ManageSubnet

	subnetID := "555"

	request := &corev1.NetworkSegmentDeletionRequest{
		Id: &corev1.NetworkSegmentId{Value: subnetID},
	}

	// Mock DeleteSubnetOnSite activity
	s.env.RegisterActivity(SubnetManager.DeleteSubnetOnSite)
	s.env.OnActivity(SubnetManager.DeleteSubnetOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	s.env.ExecuteWorkflow(DeleteSubnetV2, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *DeleteSubnetV2TestSuite) Test_DeleteSubnetV2_Failure() {
	var SubnetManager iActivity.ManageSubnet

	subnetID := "555"

	request := &corev1.NetworkSegmentDeletionRequest{
		Id: &corev1.NetworkSegmentId{Value: subnetID},
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteSubnetOnSite activity
	s.env.RegisterActivity(SubnetManager.DeleteSubnetOnSite)
	s.env.OnActivity(SubnetManager.DeleteSubnetOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute DeleteSubnet workflow
	s.env.ExecuteWorkflow(DeleteSubnetV2, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestDeleteSubnetV2TestSuite(t *testing.T) {
	suite.Run(t, new(DeleteSubnetV2TestSuite))
}

type InventorySubnetTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ivts *InventorySubnetTestSuite) SetupTest() {
	ivts.env = ivts.NewTestWorkflowEnvironment()
}

func (ivts *InventorySubnetTestSuite) AfterTest(suiteName, testName string) {
	ivts.env.AssertExpectations(ivts.T())
}

func (ivts *InventorySubnetTestSuite) Test_DiscoverSubnetInventory_Success() {
	var inventoryManager iActivity.ManageSubnetInventory

	ivts.env.RegisterActivity(inventoryManager.DiscoverSubnetInventory)
	ivts.env.OnActivity(inventoryManager.DiscoverSubnetInventory, mock.Anything).Return(nil)

	// execute workflow
	ivts.env.ExecuteWorkflow(DiscoverSubnetInventory)
	ivts.True(ivts.env.IsWorkflowCompleted())
	ivts.NoError(ivts.env.GetWorkflowError())
}

func (ivts *InventorySubnetTestSuite) Test_DiscoverSubnetInventory_ActivityFails() {
	var inventoryManager iActivity.ManageSubnetInventory

	errMsg := "Site Controller communication error"

	ivts.env.RegisterActivity(inventoryManager.DiscoverSubnetInventory)
	ivts.env.OnActivity(inventoryManager.DiscoverSubnetInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	ivts.env.ExecuteWorkflow(DiscoverSubnetInventory)
	ivts.True(ivts.env.IsWorkflowCompleted())
	err := ivts.env.GetWorkflowError()
	ivts.Error(err)

	var applicationErr *temporal.ApplicationError
	ivts.True(errors.As(err, &applicationErr))
	ivts.Equal(errMsg, applicationErr.Error())
}

func TestInventorySubnetTestSuite(t *testing.T) {
	suite.Run(t, new(InventorySubnetTestSuite))
}
