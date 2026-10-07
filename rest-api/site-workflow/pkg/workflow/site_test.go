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

type InventorySiteConfigTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (iscts *InventorySiteConfigTestSuite) SetupTest() {
	iscts.env = iscts.NewTestWorkflowEnvironment()
}

func (iscts *InventorySiteConfigTestSuite) AfterTest(suiteName, testName string) {
	iscts.env.AssertExpectations(iscts.T())
}

func (iscts *InventorySiteConfigTestSuite) Test_DiscoverSiteConfigInventory_Success() {
	var inventoryManager iActivity.ManageSiteConfigInventory

	iscts.env.RegisterActivity(inventoryManager.DiscoverSiteConfigInventory)
	iscts.env.OnActivity(inventoryManager.DiscoverSiteConfigInventory, mock.Anything).Return(nil)

	iscts.env.ExecuteWorkflow(DiscoverSiteConfigInventory)
	iscts.True(iscts.env.IsWorkflowCompleted())
	iscts.NoError(iscts.env.GetWorkflowError())
}

func (iscts *InventorySiteConfigTestSuite) Test_DiscoverSiteConfigInventory_ActivityFails() {
	var inventoryManager iActivity.ManageSiteConfigInventory

	errMsg := "Site Controller communication error"

	iscts.env.RegisterActivity(inventoryManager.DiscoverSiteConfigInventory)
	iscts.env.OnActivity(inventoryManager.DiscoverSiteConfigInventory, mock.Anything).Return(errors.New(errMsg))

	iscts.env.ExecuteWorkflow(DiscoverSiteConfigInventory)
	iscts.True(iscts.env.IsWorkflowCompleted())
	err := iscts.env.GetWorkflowError()
	iscts.Error(err)

	var applicationErr *temporal.ApplicationError
	iscts.True(errors.As(err, &applicationErr))
	iscts.Equal(errMsg, applicationErr.Error())
}

func TestInventorySiteConfigTestSuite(t *testing.T) {
	suite.Run(t, new(InventorySiteConfigTestSuite))
}
