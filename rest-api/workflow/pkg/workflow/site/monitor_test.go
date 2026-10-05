// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	siteActivity "github.com/dsx-ai-factory/infra-controller/rest-api/workflow/pkg/activity/site"
	tmocks "go.temporal.io/sdk/mocks"
)

type MonitorHealthForAllSitesTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *MonitorHealthForAllSitesTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *MonitorHealthForAllSitesTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *MonitorHealthForAllSitesTestSuite) Test_MonitorHealthForAllSitesWorkflow_Success() {
	var siteManager siteActivity.ManageSite

	// Mock MonitorInventoryReceiptForAllSites activity success
	s.env.RegisterActivity(siteManager.MonitorInventoryReceiptForAllSites)
	s.env.OnActivity(siteManager.MonitorInventoryReceiptForAllSites, mock.Anything).Return(nil)

	// execute MonitorHealthForAllSites workflow
	s.env.ExecuteWorkflow(MonitorHealthForAllSites)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *MonitorHealthForAllSitesTestSuite) Test_MonitorHealthForAllSitesWorkflow_ActivityFails() {
	var siteManager siteActivity.ManageSite

	// Mock MonitorInventoryReceiptForAllSites activity failure
	s.env.RegisterActivity(siteManager.MonitorInventoryReceiptForAllSites)
	s.env.OnActivity(siteManager.MonitorInventoryReceiptForAllSites, mock.Anything).Return(errors.New("MonitorInventoryReceiptForAllSites Failure"))

	// Execute MonitorHealthForAllSites workflow
	s.env.ExecuteWorkflow(MonitorHealthForAllSites)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("MonitorInventoryReceiptForAllSites Failure", applicationErr.Error())
}

func (s *MonitorHealthForAllSitesTestSuite) Test_ExecuteMonitorHealthForAllSitesWorkflow_Success() {
	ctx := context.Background()

	wrid := "test-workflow-run-id"

	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wrid)

	tc := &tmocks.Client{}

	tc.Mock.On("ExecuteWorkflow", context.Background(), mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.Anything).Return(wrun, nil)

	rwrid, err := ExecuteMonitorHealthForAllSitesWorkflow(ctx, tc)
	s.NoError(err)
	s.Equal(wrid, *rwrid)
}

func TestMonitorHealthForAllSitesSuite(t *testing.T) {
	suite.Run(t, new(MonitorHealthForAllSitesTestSuite))
}

type MonitorCertExpirationTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (s *MonitorCertExpirationTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *MonitorCertExpirationTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *MonitorCertExpirationTestSuite) Test_MonitorCertExpirationWorkflow_Success() {
	var siteManager siteActivity.ManageSite

	// Mock CheckOTPExpirationAndRenewForAllSites activity success
	s.env.RegisterActivity(siteManager.CheckOTPExpirationAndRenewForAllSites)
	s.env.OnActivity(siteManager.CheckOTPExpirationAndRenewForAllSites, mock.Anything).Return(nil)

	// Execute MonitorTemporalCertExpirationForAllSites workflow
	s.env.ExecuteWorkflow(MonitorTemporalCertExpirationForAllSites)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *MonitorCertExpirationTestSuite) Test_MonitorCertExpirationWorkflow_ActivityFails() {
	var siteManager siteActivity.ManageSite

	// Mock CheckOTPExpirationAndRenewForAllSites activity failure. The error is retryable, so a single
	// call shows the workflow does not retry the activity.
	s.env.RegisterActivity(siteManager.CheckOTPExpirationAndRenewForAllSites)
	s.env.OnActivity(siteManager.CheckOTPExpirationAndRenewForAllSites, mock.Anything).Return(errors.New("CheckOTPExpirationAndRenewForAllSites Failure"))

	// Execute MonitorTemporalCertExpirationForAllSites workflow
	s.env.ExecuteWorkflow(MonitorTemporalCertExpirationForAllSites)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("CheckOTPExpirationAndRenewForAllSites Failure", applicationErr.Error())
	s.env.AssertActivityNumberOfCalls(s.T(), "CheckOTPExpirationAndRenewForAllSites", 1)
}

func (s *MonitorCertExpirationTestSuite) Test_ExecuteMonitorCertExpirationWorkflow_Success() {
	ctx := context.Background()

	wrid := "test-workflow-run-id"

	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wrid)

	tc := &tmocks.Client{}
	tc.Mock.On("ExecuteWorkflow", context.Background(), mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.Anything).Return(wrun, nil)

	// Use ExecuteMonitorTemporalCertExpirationForAllSites with the mock client
	rwrid, err := ExecuteMonitorTemporalCertExpirationForAllSites(ctx, tc)
	s.NoError(err)
	s.Equal(wrid, *rwrid)
}

func TestMonitorCertExpirationTestSuite(t *testing.T) {
	suite.Run(t, new(MonitorCertExpirationTestSuite))
}

type UpdateAgentCertExpiryWorkflowTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite
	env *testsuite.TestWorkflowEnvironment
}

func (s *UpdateAgentCertExpiryWorkflowTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *UpdateAgentCertExpiryWorkflowTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *UpdateAgentCertExpiryWorkflowTestSuite) Test_UpdateAgentCertExpiryWorkflow_Success() {
	var siteManager siteActivity.ManageSite

	// Mock activity: UpdateAgentCertExpiry
	s.env.RegisterActivity(siteManager.UpdateAgentCertExpiry)
	s.env.OnActivity(siteManager.UpdateAgentCertExpiry, mock.Anything, mock.AnythingOfType("uuid.UUID"), mock.AnythingOfType("time.Time")).
		Return(nil)

	siteID := uuid.New().String()
	certExpiry := time.Now().Add(24 * time.Hour)

	s.env.ExecuteWorkflow(UpdateAgentCertExpiry, siteID, certExpiry)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *UpdateAgentCertExpiryWorkflowTestSuite) Test_UpdateAgentCertExpiryWorkflow_ActivityFails() {
	var siteManager siteActivity.ManageSite

	// Mock activity: UpdateAgentCertExpiry returns error
	s.env.RegisterActivity(siteManager.UpdateAgentCertExpiry)
	s.env.OnActivity(siteManager.UpdateAgentCertExpiry, mock.Anything, mock.AnythingOfType("uuid.UUID"), mock.AnythingOfType("time.Time")).
		Return(errors.New("Failed to update AgentCertExpiry"))

	siteID := uuid.New().String()
	certExpiry := time.Now().Add(24 * time.Hour)

	s.env.ExecuteWorkflow(UpdateAgentCertExpiry, siteID, certExpiry)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("Failed to update AgentCertExpiry", applicationErr.Error())
}

func TestUpdateAgentCertExpiryWorkflowTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateAgentCertExpiryWorkflowTestSuite))
}

type MonitorSiteTemporalNamespacesWorkflowTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite
	env *testsuite.TestWorkflowEnvironment
}

func (s *MonitorSiteTemporalNamespacesWorkflowTestSuite) SetupTest() {
	s.env = s.NewTestWorkflowEnvironment()
}

func (s *MonitorSiteTemporalNamespacesWorkflowTestSuite) AfterTest(suiteName, testName string) {
	s.env.AssertExpectations(s.T())
}

func (s *MonitorSiteTemporalNamespacesWorkflowTestSuite) Test_MonitorSiteTemporalNamespacesWorkflow_Success() {
	var siteManager siteActivity.ManageSite

	// Mock activity: MonitorSiteTemporalNamespaces
	s.env.RegisterActivity(siteManager.DeleteOrphanedSiteTemporalNamespaces)
	s.env.OnActivity(siteManager.DeleteOrphanedSiteTemporalNamespaces, mock.Anything).Return(nil)

	s.env.ExecuteWorkflow(MonitorSiteTemporalNamespaces)
	s.True(s.env.IsWorkflowCompleted())
	s.NoError(s.env.GetWorkflowError())
}

func (s *MonitorSiteTemporalNamespacesWorkflowTestSuite) Test_MonitorSiteTemporalNamespacesWorkflow_ActivityFails() {
	var siteManager siteActivity.ManageSite

	// Mock activity: MonitorSiteTemporalNamespaces returns error
	s.env.RegisterActivity(siteManager.DeleteOrphanedSiteTemporalNamespaces)
	s.env.OnActivity(siteManager.DeleteOrphanedSiteTemporalNamespaces, mock.Anything).
		Return(errors.New("Failed to update AgentCertExpiry"))

	s.env.ExecuteWorkflow(MonitorSiteTemporalNamespaces)
	s.True(s.env.IsWorkflowCompleted())
	err := s.env.GetWorkflowError()
	s.Error(err)

	var applicationErr *temporal.ApplicationError
	s.True(errors.As(err, &applicationErr))
	s.Equal("Failed to update AgentCertExpiry", applicationErr.Error())
}

func TestMonitorSiteTemporalNamespacesWorkflowTestSuite(t *testing.T) {
	suite.Run(t, new(MonitorSiteTemporalNamespacesWorkflowTestSuite))
}
