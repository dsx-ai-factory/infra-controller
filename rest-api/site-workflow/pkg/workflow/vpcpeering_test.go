// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
)

type CreateVpcPeeringTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *CreateVpcPeeringTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *CreateVpcPeeringTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *CreateVpcPeeringTestSuite) Test_CreateVpcPeering_Success() {
	var manager iActivity.ManageVpcPeering
	request := &corev1.VpcPeeringCreationRequest{
		VpcId:     &corev1.VpcId{Value: uuid.NewString()},
		PeerVpcId: &corev1.VpcId{Value: uuid.NewString()},
	}

	// Mock CreateVpcPeeringOnSite activity
	s.env.RegisterActivity(manager.CreateVpcPeeringOnSite)
	s.env.OnActivity(manager.CreateVpcPeeringOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute CreateVpcPeering workflow
	s.env.ExecuteWorkflow(CreateVpcPeering, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *CreateVpcPeeringTestSuite) Test_CreateVpcPeering_Failure() {
	var manager iActivity.ManageVpcPeering
	request := &corev1.VpcPeeringCreationRequest{
		VpcId:     &corev1.VpcId{Value: uuid.NewString()},
		PeerVpcId: &corev1.VpcId{Value: uuid.NewString()},
	}

	errMsg := "Site Controller communication error"

	// Mock CreateVpcPeeringOnSite activity
	s.env.RegisterActivity(manager.CreateVpcPeeringOnSite)
	s.env.OnActivity(manager.CreateVpcPeeringOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// Execute CreateVpcPeering workflow
	s.env.ExecuteWorkflow(CreateVpcPeering, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestCreateVpcPeeringTestSuite(t *testing.T) {
	suite.Run(t, new(CreateVpcPeeringTestSuite))
}

type DeleteVpcPeeringTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *DeleteVpcPeeringTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *DeleteVpcPeeringTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *DeleteVpcPeeringTestSuite) Test_DeleteVpcPeering_Success() {
	var vpcPeeringManager iActivity.ManageVpcPeering

	request := &corev1.VpcPeeringDeletionRequest{
		Id: &corev1.VpcPeeringId{Value: uuid.NewString()},
	}

	// Mock DeleteVpcPeeringOnSite activity
	s.env.RegisterActivity(vpcPeeringManager.DeleteVpcPeeringOnSite)
	s.env.OnActivity(vpcPeeringManager.DeleteVpcPeeringOnSite, mock.Anything, mock.Anything).Return(nil)

	// Execute DeleteVpcPeering workflow
	s.env.ExecuteWorkflow(DeleteVpcPeering, request)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *DeleteVpcPeeringTestSuite) Test_DeleteVpcPeering_Failure() {
	var vpcPeeringManager iActivity.ManageVpcPeering

	request := &corev1.VpcPeeringDeletionRequest{
		Id: &corev1.VpcPeeringId{Value: uuid.NewString()},
	}

	errMsg := "Site Controller communication error"

	// Mock DeleteVpcPeeringOnSite activity
	s.env.RegisterActivity(vpcPeeringManager.DeleteVpcPeeringOnSite)
	s.env.OnActivity(vpcPeeringManager.DeleteVpcPeeringOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// Execute DeleteVpcPeering workflow
	s.env.ExecuteWorkflow(DeleteVpcPeering, request)
	s.True(s.env.IsWorkflowCompleted())
	s.Error(s.env.GetWorkflowError())
}

func TestDeleteVpcPeeringTestSuite(t *testing.T) {
	suite.Run(t, new(DeleteVpcPeeringTestSuite))
}

type InventoryVpcPeeringTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *InventoryVpcPeeringTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *InventoryVpcPeeringTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *InventoryVpcPeeringTestSuite) Test_DiscoverVpcPeeringInventory_Success() {
	var inventoryManager iActivity.ManageVpcPeeringInventory

	s.env.RegisterActivity(inventoryManager.DiscoverVpcPeeringInventory)
	s.env.OnActivity(inventoryManager.DiscoverVpcPeeringInventory, mock.Anything).Return(nil)

	// Execute workflow
	s.env.ExecuteWorkflow(DiscoverVpcPeeringInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *InventoryVpcPeeringTestSuite) Test_DiscoverVpcPeeringInventory_Failure() {
	var inventoryManager iActivity.ManageVpcPeeringInventory

	errMsg := "Site Controller communication error"

	s.env.RegisterActivity(inventoryManager.DiscoverVpcPeeringInventory)
	s.env.OnActivity(inventoryManager.DiscoverVpcPeeringInventory, mock.Anything).Return(errors.New(errMsg))

	// Execute workflow
	s.env.ExecuteWorkflow(DiscoverVpcPeeringInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestInventoryVpcPeeringTestSuite(t *testing.T) {
	suite.Run(t, new(InventoryVpcPeeringTestSuite))
}
