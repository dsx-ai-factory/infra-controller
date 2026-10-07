// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sku

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	skuActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/sku"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type UpdateSkuTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateSkuTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateSkuTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateSkuTestSuite) Test_UpdateSkuInventory_Success() {
	var skuManager skuActivity.ManageSku

	siteID := uuid.New()

	inv := &corev1.SkuInventory{Skus: []*corev1.Sku{}}

	s.env.RegisterActivity(skuManager.UpdateSkusInDB)
	s.env.OnActivity(skuManager.UpdateSkusInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	s.env.ExecuteWorkflow(UpdateSkuInventory, siteID.String(), inv)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateSkuTestSuite) Test_UpdateSkuInventory_ActivityFails() {
	var skuManager skuActivity.ManageSku

	siteID := uuid.New()

	inv := &corev1.SkuInventory{Skus: []*corev1.Sku{}}

	s.env.RegisterActivity(skuManager.UpdateSkusInDB)
	s.env.OnActivity(skuManager.UpdateSkusInDB, mock.Anything, mock.Anything, mock.Anything).Return(errors.New("UpdateSkuInventory Failure"))

	s.env.ExecuteWorkflow(UpdateSkuInventory, siteID.String(), inv)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.NotNil(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateSkuInventory Failure", applicationErr.Error())
}

func TestUpdateSkuTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateSkuTestSuite))
}
