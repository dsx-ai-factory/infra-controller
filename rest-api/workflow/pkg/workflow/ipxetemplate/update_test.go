// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ipxetemplate

import (
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	ipxeTemplateActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/ipxetemplate"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type UpdateIpxeTemplateTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateIpxeTemplateTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateIpxeTemplateTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateIpxeTemplateTestSuite) Test_UpdateIpxeTemplateInventory_Success() {
	var templateManager ipxeTemplateActivity.ManageIpxeTemplate
	var metricsManager cwm.ManageInventoryMetrics

	siteID := uuid.New()
	inv := &corev1.IpxeTemplateInventory{Templates: []*corev1.IpxeTemplate{}}

	s.env.RegisterActivity(templateManager.UpdateIpxeTemplatesInDB)
	s.env.OnActivity(templateManager.UpdateIpxeTemplatesInDB, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	s.env.RegisterActivity(metricsManager.RecordLatency)
	s.env.OnActivity(metricsManager.RecordLatency, mock.Anything, mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	s.env.ExecuteWorkflow(UpdateIpxeTemplateInventory, siteID.String(), inv)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateIpxeTemplateTestSuite) Test_UpdateIpxeTemplateInventory_ActivityFails() {
	var templateManager ipxeTemplateActivity.ManageIpxeTemplate
	var metricsManager cwm.ManageInventoryMetrics

	siteID := uuid.New()
	inv := &corev1.IpxeTemplateInventory{Templates: []*corev1.IpxeTemplate{}}

	s.env.RegisterActivity(templateManager.UpdateIpxeTemplatesInDB)
	s.env.OnActivity(templateManager.UpdateIpxeTemplatesInDB, mock.Anything, mock.Anything, mock.Anything).Return(errors.New("UpdateIpxeTemplatesInDB failure"))

	s.env.RegisterActivity(metricsManager.RecordLatency)
	s.env.OnActivity(metricsManager.RecordLatency, mock.Anything, mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil).Maybe()

	s.env.ExecuteWorkflow(UpdateIpxeTemplateInventory, siteID.String(), inv)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.NotNil(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("UpdateIpxeTemplatesInDB failure", applicationErr.Error())
}

func (s *UpdateIpxeTemplateTestSuite) Test_UpdateIpxeTemplateInventory_InvalidSiteID() {
	inv := &corev1.IpxeTemplateInventory{Templates: []*corev1.IpxeTemplate{}}

	s.env.ExecuteWorkflow(UpdateIpxeTemplateInventory, "not-a-valid-uuid", inv)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.NotNil(err)
}

func TestUpdateIpxeTemplateTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateIpxeTemplateTestSuite))
}
