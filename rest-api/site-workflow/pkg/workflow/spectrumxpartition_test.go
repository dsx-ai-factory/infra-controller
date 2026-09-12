// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"

	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
)

type InventorySpectrumXPartitionTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *InventorySpectrumXPartitionTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *InventorySpectrumXPartitionTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *InventorySpectrumXPartitionTestSuite) Test_DiscoverSpectrumXPartitionInventory_Success() {
	var inventoryManager iActivity.ManageSpectrumXPartitionInventory

	s.env.RegisterActivity(inventoryManager.DiscoverSpectrumXPartitionInventory)
	s.env.OnActivity(inventoryManager.DiscoverSpectrumXPartitionInventory, mock.Anything).Return(nil)

	s.env.ExecuteWorkflow(DiscoverSpectrumXPartitionInventory)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

// A collection failure has to surface as a workflow error rather than completing
// successfully, or the cron keeps running with nothing signalling the outage.
func (s *InventorySpectrumXPartitionTestSuite) Test_DiscoverSpectrumXPartitionInventory_ActivityFails() {
	var inventoryManager iActivity.ManageSpectrumXPartitionInventory

	errMsg := "Site Controller communication error"

	s.env.RegisterActivity(inventoryManager.DiscoverSpectrumXPartitionInventory)
	s.env.OnActivity(inventoryManager.DiscoverSpectrumXPartitionInventory, mock.Anything).Return(errors.New(errMsg))

	s.env.ExecuteWorkflow(DiscoverSpectrumXPartitionInventory)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal(errMsg, applicationErr.Error())
}

func TestInventorySpectrumXPartitionTestSuite(t *testing.T) {
	suite.Run(t, new(InventorySpectrumXPartitionTestSuite))
}
