// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	iActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/suite"
	"go.temporal.io/sdk/testsuite"
)

type EnableDisableMachineValidationTestTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ts *EnableDisableMachineValidationTestTestSuite) SetupTest() {
	ts.env = ts.NewTestWorkflowEnvironment()
}

func (ts *EnableDisableMachineValidationTestTestSuite) AfterTest(suiteName, testName string) {
	ts.env.AssertExpectations(ts.T())
}

func (ts *EnableDisableMachineValidationTestTestSuite) Test_Success() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationTestEnableDisableTestRequest{
		TestId:    "test-id-1",
		Version:   "test-version-1",
		IsEnabled: true,
	}

	// mock activity
	ts.env.RegisterActivity(manager.EnableDisableMachineValidationTestOnSite)
	ts.env.OnActivity(manager.EnableDisableMachineValidationTestOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	ts.env.ExecuteWorkflow(EnableDisableMachineValidationTest, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.NoError(ts.env.GetWorkflowError())
}

func (ts *EnableDisableMachineValidationTestTestSuite) Test_Failure() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationTestEnableDisableTestRequest{
		TestId:    "test-id-1",
		Version:   "test-version-1",
		IsEnabled: true,
	}

	errMsg := "site controller communication error"

	// mock activity
	ts.env.RegisterActivity(manager.EnableDisableMachineValidationTestOnSite)
	ts.env.OnActivity(manager.EnableDisableMachineValidationTestOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute workflow
	ts.env.ExecuteWorkflow(EnableDisableMachineValidationTest, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.Error(ts.env.GetWorkflowError())
}

func TestEnableDisableMachineValidationTestTestSuite(t *testing.T) {
	suite.Run(t, new(EnableDisableMachineValidationTestTestSuite))
}

type PersistValidationResultTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ts *PersistValidationResultTestSuite) SetupTest() {
	ts.env = ts.NewTestWorkflowEnvironment()
}

func (ts *PersistValidationResultTestSuite) AfterTest(suiteName, testName string) {
	ts.env.AssertExpectations(ts.T())
}

func (ts *PersistValidationResultTestSuite) Test_Success() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationResultPostRequest{
		Result: &corev1.MachineValidationResult{
			Name: "test-1",
		},
	}

	// mock activity
	ts.env.RegisterActivity(manager.PersistValidationResultOnSite)
	ts.env.OnActivity(manager.PersistValidationResultOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	ts.env.ExecuteWorkflow(PersistValidationResult, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.NoError(ts.env.GetWorkflowError())
}

func (ts *PersistValidationResultTestSuite) Test_Failure() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationResultPostRequest{
		Result: &corev1.MachineValidationResult{
			Name: "test-1",
		},
	}

	errMsg := "site controller communication error"

	// mock activity
	ts.env.RegisterActivity(manager.PersistValidationResultOnSite)
	ts.env.OnActivity(manager.PersistValidationResultOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute workflow
	ts.env.ExecuteWorkflow(PersistValidationResult, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.Error(ts.env.GetWorkflowError())
}

func TestPersistValidationResultTestSuite(t *testing.T) {
	suite.Run(t, new(PersistValidationResultTestSuite))
}

type GetMachineValidationResultsTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ts *GetMachineValidationResultsTestSuite) SetupTest() {
	ts.env = ts.NewTestWorkflowEnvironment()
}

func (ts *GetMachineValidationResultsTestSuite) AfterTest(suiteName, testName string) {
	ts.env.AssertExpectations(ts.T())
}

func (ts *GetMachineValidationResultsTestSuite) Test_Success() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationGetRequest{
		MachineId: &corev1.MachineId{
			Id: "machine-id-1",
		},
	}

	mockResponse := &corev1.MachineValidationResultList{
		Results: []*corev1.MachineValidationResult{
			{
				Name: "test-1",
			},
		},
	}

	// mock activity
	ts.env.RegisterActivity(manager.GetMachineValidationResultsFromSite)
	ts.env.OnActivity(manager.GetMachineValidationResultsFromSite, mock.Anything, mock.Anything).Return(mockResponse, nil)

	// execute workflow
	ts.env.ExecuteWorkflow(GetMachineValidationResults, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.NoError(ts.env.GetWorkflowError())

	// get actual response and validate
	var actualResponse corev1.MachineValidationResultList
	if err := ts.env.GetWorkflowResult(&actualResponse); err != nil {
		ts.Fail(err.Error())
	}
	ts.Equal(len(mockResponse.Results), len(actualResponse.Results))
	ts.Equal(mockResponse.Results[0].Name, actualResponse.Results[0].Name)
}

func (ts *GetMachineValidationResultsTestSuite) Test_Failure() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationGetRequest{
		MachineId: &corev1.MachineId{
			Id: "machine-id-1",
		},
	}

	errMsg := "site controller communication error"

	// mock activity
	ts.env.RegisterActivity(manager.GetMachineValidationResultsFromSite)
	ts.env.OnActivity(manager.GetMachineValidationResultsFromSite, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// execute workflow
	ts.env.ExecuteWorkflow(GetMachineValidationResults, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.Error(ts.env.GetWorkflowError())
}

func TestGetMachineValidationResultsTestSuite(t *testing.T) {
	suite.Run(t, new(GetMachineValidationResultsTestSuite))
}

type GetMachineValidationRunsTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ts *GetMachineValidationRunsTestSuite) SetupTest() {
	ts.env = ts.NewTestWorkflowEnvironment()
}

func (ts *GetMachineValidationRunsTestSuite) AfterTest(suiteName, testName string) {
	ts.env.AssertExpectations(ts.T())
}

func (ts *GetMachineValidationRunsTestSuite) Test_Success() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationRunListGetRequest{
		MachineId: &corev1.MachineId{
			Id: "machine-id-1",
		},
	}

	mockResponse := &corev1.MachineValidationRunList{
		Runs: []*corev1.MachineValidationRun{
			{
				Name: "test-1",
			},
		},
	}

	// mock activity
	ts.env.RegisterActivity(manager.GetMachineValidationRunsFromSite)
	ts.env.OnActivity(manager.GetMachineValidationRunsFromSite, mock.Anything, mock.Anything).Return(mockResponse, nil)

	// execute workflow
	ts.env.ExecuteWorkflow(GetMachineValidationRuns, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.NoError(ts.env.GetWorkflowError())

	// get actual response and validate
	var actualResponse corev1.MachineValidationRunList
	if err := ts.env.GetWorkflowResult(&actualResponse); err != nil {
		ts.Fail(err.Error())
	}
	ts.Equal(len(mockResponse.Runs), len(actualResponse.Runs))
	ts.Equal(mockResponse.Runs[0].Name, actualResponse.Runs[0].Name)
}

func (ts *GetMachineValidationRunsTestSuite) Test_Failure() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationRunListGetRequest{
		MachineId: &corev1.MachineId{
			Id: "machine-id-1",
		},
	}

	errMsg := "site controller communication error"

	// mock activity
	ts.env.RegisterActivity(manager.GetMachineValidationRunsFromSite)
	ts.env.OnActivity(manager.GetMachineValidationRunsFromSite, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// execute workflow
	ts.env.ExecuteWorkflow(GetMachineValidationRuns, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.Error(ts.env.GetWorkflowError())
}

func TestGetMachineValidationRunsTestSuite(t *testing.T) {
	suite.Run(t, new(GetMachineValidationRunsTestSuite))
}

type GetMachineValidationTestsTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ts *GetMachineValidationTestsTestSuite) SetupTest() {
	ts.env = ts.NewTestWorkflowEnvironment()
}

func (ts *GetMachineValidationTestsTestSuite) AfterTest(suiteName, testName string) {
	ts.env.AssertExpectations(ts.T())
}

func (ts *GetMachineValidationTestsTestSuite) Test_Success() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationTestsGetRequest{}

	mockResponse := &corev1.MachineValidationTestsGetResponse{
		Tests: []*corev1.MachineValidationTest{
			{
				Name: "test-1",
			},
		},
	}

	// mock activity
	ts.env.RegisterActivity(manager.GetMachineValidationTestsFromSite)
	ts.env.OnActivity(manager.GetMachineValidationTestsFromSite, mock.Anything, mock.Anything).Return(mockResponse, nil)

	// execute workflow
	ts.env.ExecuteWorkflow(GetMachineValidationTests, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.NoError(ts.env.GetWorkflowError())

	// get actual response and validate
	var actualResponse corev1.MachineValidationTestsGetResponse
	if err := ts.env.GetWorkflowResult(&actualResponse); err != nil {
		ts.Fail(err.Error())
	}
	ts.Equal(len(mockResponse.Tests), len(actualResponse.Tests))
	ts.Equal(mockResponse.Tests[0].Name, actualResponse.Tests[0].Name)
}

func (ts *GetMachineValidationTestsTestSuite) Test_Failure() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationTestsGetRequest{}

	errMsg := "site controller communication error"

	// mock activity
	ts.env.RegisterActivity(manager.GetMachineValidationTestsFromSite)
	ts.env.OnActivity(manager.GetMachineValidationTestsFromSite, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// execute workflow
	ts.env.ExecuteWorkflow(GetMachineValidationTests, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.Error(ts.env.GetWorkflowError())
}

func TestGetMachineValidationTestsTestSuite(t *testing.T) {
	suite.Run(t, new(GetMachineValidationTestsTestSuite))
}

type AddMachineValidationTestTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ts *AddMachineValidationTestTestSuite) SetupTest() {
	ts.env = ts.NewTestWorkflowEnvironment()
}

func (ts *AddMachineValidationTestTestSuite) AfterTest(suiteName, testName string) {
	ts.env.AssertExpectations(ts.T())
}

func (ts *AddMachineValidationTestTestSuite) Test_Success() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationTestAddRequest{
		Name:    "test-1",
		Command: "test-command",
		Args:    "test-args",
	}

	mockResponse := &corev1.MachineValidationTestAddUpdateResponse{
		TestId:  "test-id-1",
		Version: "test-version-1",
	}

	// mock activity
	ts.env.RegisterActivity(manager.AddMachineValidationTestOnSite)
	ts.env.OnActivity(manager.AddMachineValidationTestOnSite, mock.Anything, mock.Anything).Return(mockResponse, nil)

	// execute workflow
	ts.env.ExecuteWorkflow(AddMachineValidationTest, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.NoError(ts.env.GetWorkflowError())

	// get actual response and validate
	var actualResponse corev1.MachineValidationTestAddUpdateResponse
	if err := ts.env.GetWorkflowResult(&actualResponse); err != nil {
		ts.Fail(err.Error())
	}
	ts.Equal(mockResponse.TestId, actualResponse.TestId)
	ts.Equal(mockResponse.Version, actualResponse.Version)
}

func (ts *AddMachineValidationTestTestSuite) Test_Failure() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationTestAddRequest{
		Name:    "test-1",
		Command: "test-command",
		Args:    "test-args",
	}

	errMsg := "site controller communication error"

	// mock activity
	ts.env.RegisterActivity(manager.AddMachineValidationTestOnSite)
	ts.env.OnActivity(manager.AddMachineValidationTestOnSite, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// execute workflow
	ts.env.ExecuteWorkflow(AddMachineValidationTest, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.Error(ts.env.GetWorkflowError())
}

func TestAddMachineValidationTestTestSuite(t *testing.T) {
	suite.Run(t, new(AddMachineValidationTestTestSuite))
}

type UpdateMachineValidationTestTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ts *UpdateMachineValidationTestTestSuite) SetupTest() {
	ts.env = ts.NewTestWorkflowEnvironment()
}

func (ts *UpdateMachineValidationTestTestSuite) AfterTest(suiteName, testName string) {
	ts.env.AssertExpectations(ts.T())
}

func (ts *UpdateMachineValidationTestTestSuite) Test_Success() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationTestUpdateRequest{
		TestId:  "test-1",
		Version: "version-1",
		Payload: &corev1.MachineValidationTestUpdateRequest_Payload{
			Name: util.GetStrPtr("name-2"),
		},
	}

	// mock activity
	ts.env.RegisterActivity(manager.UpdateMachineValidationTestOnSite)
	ts.env.OnActivity(manager.UpdateMachineValidationTestOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	ts.env.ExecuteWorkflow(UpdateMachineValidationTest, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.NoError(ts.env.GetWorkflowError())
}

func (ts *UpdateMachineValidationTestTestSuite) Test_Failure() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.MachineValidationTestUpdateRequest{
		TestId:  "test-1",
		Version: "version-1",
		Payload: &corev1.MachineValidationTestUpdateRequest_Payload{
			Name: util.GetStrPtr("name-2"),
		},
	}

	errMsg := "site controller communication error"

	// mock activity
	ts.env.RegisterActivity(manager.UpdateMachineValidationTestOnSite)
	ts.env.OnActivity(manager.UpdateMachineValidationTestOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute workflow
	ts.env.ExecuteWorkflow(UpdateMachineValidationTest, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.Error(ts.env.GetWorkflowError())
}

func TestUpdateMachineValidationTestTestSuite(t *testing.T) {
	suite.Run(t, new(UpdateMachineValidationTestTestSuite))
}

type GetMachineValidationExternalConfigsTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ts *GetMachineValidationExternalConfigsTestSuite) SetupTest() {
	ts.env = ts.NewTestWorkflowEnvironment()
}

func (ts *GetMachineValidationExternalConfigsTestSuite) AfterTest(suiteName, testName string) {
	ts.env.AssertExpectations(ts.T())
}

func (ts *GetMachineValidationExternalConfigsTestSuite) Test_Success() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.GetMachineValidationExternalConfigsRequest{}

	mockResponse := &corev1.GetMachineValidationExternalConfigsResponse{
		Configs: []*corev1.MachineValidationExternalConfig{
			{
				Name:    "config-1",
				Version: "version-1",
			},
		},
	}

	// mock activity
	ts.env.RegisterActivity(manager.GetMachineValidationExternalConfigsFromSite)
	ts.env.OnActivity(manager.GetMachineValidationExternalConfigsFromSite, mock.Anything, mock.Anything).Return(mockResponse, nil)

	// execute workflow
	ts.env.ExecuteWorkflow(GetMachineValidationExternalConfigs, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.NoError(ts.env.GetWorkflowError())

	// get actual response and validate
	var actualResponse corev1.GetMachineValidationExternalConfigsResponse
	if err := ts.env.GetWorkflowResult(&actualResponse); err != nil {
		ts.Fail(err.Error())
	}
	ts.Equal(len(mockResponse.Configs), len(actualResponse.Configs))
	ts.Equal(mockResponse.Configs[0].Name, actualResponse.Configs[0].Name)
}

func (ts *GetMachineValidationExternalConfigsTestSuite) Test_Failure() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.GetMachineValidationExternalConfigsRequest{}

	errMsg := "site controller communication error"

	// mock activity
	ts.env.RegisterActivity(manager.GetMachineValidationExternalConfigsFromSite)
	ts.env.OnActivity(manager.GetMachineValidationExternalConfigsFromSite, mock.Anything, mock.Anything).Return(nil, errors.New(errMsg))

	// execute workflow
	ts.env.ExecuteWorkflow(GetMachineValidationExternalConfigs, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.Error(ts.env.GetWorkflowError())
}

func TestGetMachineValidationExternalConfigsTestSuite(t *testing.T) {
	suite.Run(t, new(GetMachineValidationExternalConfigsTestSuite))
}

type AddUpdateMachineValidationExternalConfigTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ts *AddUpdateMachineValidationExternalConfigTestSuite) SetupTest() {
	ts.env = ts.NewTestWorkflowEnvironment()
}

func (ts *AddUpdateMachineValidationExternalConfigTestSuite) AfterTest(suiteName, testName string) {
	ts.env.AssertExpectations(ts.T())
}

func (ts *AddUpdateMachineValidationExternalConfigTestSuite) Test_Success() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.AddUpdateMachineValidationExternalConfigRequest{
		Name: "config-1",
	}

	// mock activity
	ts.env.RegisterActivity(manager.AddUpdateMachineValidationExternalConfigOnSite)
	ts.env.OnActivity(manager.AddUpdateMachineValidationExternalConfigOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	ts.env.ExecuteWorkflow(AddUpdateMachineValidationExternalConfig, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.NoError(ts.env.GetWorkflowError())
}

func (ts *AddUpdateMachineValidationExternalConfigTestSuite) Test_Failure() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.AddUpdateMachineValidationExternalConfigRequest{
		Name: "config-1",
	}

	errMsg := "site controller communication error"

	// mock activity
	ts.env.RegisterActivity(manager.AddUpdateMachineValidationExternalConfigOnSite)
	ts.env.OnActivity(manager.AddUpdateMachineValidationExternalConfigOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute workflow
	ts.env.ExecuteWorkflow(AddUpdateMachineValidationExternalConfig, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.Error(ts.env.GetWorkflowError())
}

func TestAddUpdateMachineValidationExternalConfigTestSuite(t *testing.T) {
	suite.Run(t, new(AddUpdateMachineValidationExternalConfigTestSuite))
}

type RemoveMachineValidationExternalConfigTestSuite struct {
	suite.Suite
	testsuite.WorkflowTestSuite

	env *testsuite.TestWorkflowEnvironment
}

func (ts *RemoveMachineValidationExternalConfigTestSuite) SetupTest() {
	ts.env = ts.NewTestWorkflowEnvironment()
}

func (ts *RemoveMachineValidationExternalConfigTestSuite) AfterTest(suiteName, testName string) {
	ts.env.AssertExpectations(ts.T())
}

func (ts *RemoveMachineValidationExternalConfigTestSuite) Test_Success() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.RemoveMachineValidationExternalConfigRequest{
		Name: "config-1",
	}

	// mock activity
	ts.env.RegisterActivity(manager.RemoveMachineValidationExternalConfigOnSite)
	ts.env.OnActivity(manager.RemoveMachineValidationExternalConfigOnSite, mock.Anything, mock.Anything).Return(nil)

	// execute workflow
	ts.env.ExecuteWorkflow(RemoveMachineValidationExternalConfig, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.NoError(ts.env.GetWorkflowError())
}

func (ts *RemoveMachineValidationExternalConfigTestSuite) Test_Failure() {
	var manager iActivity.ManageMachineValidation

	request := &corev1.RemoveMachineValidationExternalConfigRequest{
		Name: "config-1",
	}

	errMsg := "site controller communication error"

	// mock activity
	ts.env.RegisterActivity(manager.RemoveMachineValidationExternalConfigOnSite)
	ts.env.OnActivity(manager.RemoveMachineValidationExternalConfigOnSite, mock.Anything, mock.Anything).Return(errors.New(errMsg))

	// execute workflow
	ts.env.ExecuteWorkflow(RemoveMachineValidationExternalConfig, request)
	ts.True(ts.env.IsWorkflowCompleted())
	ts.Error(ts.env.GetWorkflowError())
}

func TestRemoveMachineValidationExternalConfigTestSuite(t *testing.T) {
	suite.Run(t, new(RemoveMachineValidationExternalConfigTestSuite))
}
