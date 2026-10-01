// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"go.temporal.io/api/enums/v1"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
)

func TestCreateMachineValidationTestHandler(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipRoles := []string{authz.ProviderAdminRole}

	pvu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	// test identity
	testID := "test-id-1"
	testVersion := "test-version-1"
	testName := "name"
	testCommand := "command"
	testArgs := "arg1 arg2"

	// Prepare client pool for sync calls to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// init scp client
	scpClient := &tmocks.Client{}

	// set-up create workflow
	createWorkflowRun := &tmocks.WorkflowRun{}
	createWorkflowRun.On("GetID").Return("create-workflow-id")

	createWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.MachineValidationTestAddUpdateResponse)
			*response = &corev1.MachineValidationTestAddUpdateResponse{
				TestId:  testID,
				Version: testVersion,
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"AddMachineValidationTest", mock.Anything).Return(createWorkflowRun, nil)

	// set-up get workflow
	getWorkflowID := "get-workflow-id"
	getWorkflowRun := &tmocks.WorkflowRun{}
	getWorkflowRun.On("GetID").Return(getWorkflowID)

	getWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.MachineValidationTestsGetResponse)
			*response = &corev1.MachineValidationTestsGetResponse{
				Tests: []*corev1.MachineValidationTest{
					{
						TestId:  testID,
						Version: testVersion,
						Name:    testName,
						Command: testCommand,
						Args:    testArgs,
					},
				},
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationTests", mock.Anything).Return(getWorkflowRun, nil)

	// scp client with timeout
	scpClientWithTimeout := &tmocks.Client{}

	timeoutWorkflowRun := &tmocks.WorkflowRun{}
	timeoutWorkflowRun.On("GetID").Return("create-workflow-id")

	timeoutWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	scpClientWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"AddMachineValidationTest", mock.Anything).Return(timeoutWorkflowRun, nil)

	scpClientWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name           string
		reqOrgName     string
		reqBodyModel   *model.APIMachineValidationTestCreateRequest
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		scpClient      *tmocks.Client
	}{
		{
			name:       "error when user not found in request context",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationTestCreateRequest{
				Name:    testName,
				Command: testCommand,
				Args:    testArgs,
			},
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClient,
		},
		{
			name:       "error when user not found in org",
			reqOrgName: "SomeOrg",
			reqBodyModel: &model.APIMachineValidationTestCreateRequest{
				Name:    testName,
				Command: testCommand,
				Args:    testArgs,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			scpClient:      scpClient,
		},
		{
			name:       "error when request does not include Name",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationTestCreateRequest{
				Command: testCommand,
				Args:    testArgs,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			scpClient:      scpClient,
		},
		{
			name:       "error when request does not include Command",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationTestCreateRequest{
				Name: testName,
				Args: testArgs,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			scpClient:      scpClient,
		},
		{
			name:       "error when request does not include Args",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationTestCreateRequest{
				Name:    testName,
				Command: testCommand,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			scpClient:      scpClient,
		},
		{
			name:       "error when workflow times out",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationTestCreateRequest{
				Name:    testName,
				Command: testCommand,
				Args:    testArgs,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClientWithTimeout,
		},
		{
			name:       "no error",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationTestCreateRequest{
				Name:    testName,
				Command: testCommand,
				Args:    testArgs,
			},
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			scpClient:      scpClient,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// init temporal client
			scp.IDClientMap[site.ID.String()] = tc.scpClient
			// prep body
			jsonBody, err := json.Marshal(tc.reqBodyModel)
			assert.Nil(t, err)
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(jsonBody)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "siteID")
			ec.SetParamValues(tc.reqOrgName, site.ID.String())
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := CreateMachineValidationTestHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err = cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusCreated)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				apiResponse := &model.APIMachineValidationTest{}
				err := json.Unmarshal(rec.Body.Bytes(), apiResponse)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, testID, apiResponse.TestID)
				assert.Equal(t, testVersion, apiResponse.Version)
			}
		})
	}
}

func TestUpdateMachineValidationTestHandler(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipRoles := []string{authz.ProviderAdminRole}

	pvu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	// test identity
	testID := "test-id-1"
	testVersion := "test-version-13"
	testName := "name"
	testCommand := "command"
	testArgs := "arg1 arg2 arg3"

	// Prepare client pool for sync calls to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// init scp client
	scpClient := &tmocks.Client{}

	// set-up create workflow
	updateWorkflowRun := &tmocks.WorkflowRun{}
	updateWorkflowRun.On("GetID").Return("update-workflow-id")

	updateWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.MachineValidationTestAddUpdateResponse)
			*response = &corev1.MachineValidationTestAddUpdateResponse{
				TestId:  testID,
				Version: testVersion,
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateMachineValidationTest", mock.Anything).Return(updateWorkflowRun, nil)

	// set-up get workflow
	getWorkflowID := "get-workflow-id"
	getWorkflowRun := &tmocks.WorkflowRun{}
	getWorkflowRun.On("GetID").Return(getWorkflowID)

	getWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.MachineValidationTestsGetResponse)
			*response = &corev1.MachineValidationTestsGetResponse{
				Tests: []*corev1.MachineValidationTest{
					{
						TestId:  testID,
						Version: testVersion,
						Name:    testName,
						Command: testCommand,
						Args:    testArgs,
					},
				},
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationTests", mock.Anything).Return(getWorkflowRun, nil)

	// scp client with timeout
	scpClientWithTimeout := &tmocks.Client{}

	timeoutWorkflowRun := &tmocks.WorkflowRun{}
	timeoutWorkflowRun.On("GetID").Return("update-workflow-id")

	timeoutWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	scpClientWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateMachineValidationTest", mock.Anything).Return(timeoutWorkflowRun, nil)

	scpClientWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name           string
		reqOrgName     string
		reqBodyModel   *model.APIMachineValidationTestUpdateRequest
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		scpClient      *tmocks.Client
	}{
		{
			name:       "error when user not found in request context",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationTestUpdateRequest{
				Name: &testName,
			},
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClient,
		},
		{
			name:       "error when user not found in org",
			reqOrgName: "SomeOrg",
			reqBodyModel: &model.APIMachineValidationTestUpdateRequest{
				Name: &testName,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			scpClient:      scpClient,
		},
		{
			name:       "error when workflow times out",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationTestUpdateRequest{
				Name: &testName,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClientWithTimeout,
		},
		{
			name:       "no error",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationTestUpdateRequest{
				Name: &testName,
			},
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// init temporal client
			scp.IDClientMap[site.ID.String()] = tc.scpClient
			// prep body
			jsonBody, err := json.Marshal(tc.reqBodyModel)
			assert.Nil(t, err)
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(string(jsonBody)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "siteID", "id", "version")
			ec.SetParamValues(tc.reqOrgName, site.ID.String(), testID, testVersion)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := UpdateMachineValidationTestHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err = cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				apiResponse := &model.APIMachineValidationTest{}
				err := json.Unmarshal(rec.Body.Bytes(), apiResponse)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, testID, apiResponse.TestID)
				assert.Equal(t, testVersion, apiResponse.Version)
			}
		})
	}
}

func TestGetAllMachineValidationTestHandler(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipRoles := []string{authz.ProviderAdminRole}

	pvu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	// tests
	var workflowResponse []*corev1.MachineValidationTest
	for i := 0; i < 20; i++ {
		workflowResponse = append(workflowResponse, &corev1.MachineValidationTest{
			TestId:  fmt.Sprintf("test-id-%d", i),
			Version: "version-1",
		})
	}

	// Prepare client pool for sync calls to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// init scp client
	scpClient := &tmocks.Client{}

	// set-up create workflow
	updateWorkflowRun := &tmocks.WorkflowRun{}
	updateWorkflowRun.On("GetID").Return("get-all-workflow-id")

	updateWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.MachineValidationTestsGetResponse)
			*response = &corev1.MachineValidationTestsGetResponse{
				Tests: workflowResponse,
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationTests", mock.Anything).Return(updateWorkflowRun, nil)

	// scp client with timeout
	scpClientWithTimeout := &tmocks.Client{}

	timeoutWorkflowRun := &tmocks.WorkflowRun{}
	timeoutWorkflowRun.On("GetID").Return("get-all-workflow-id")

	timeoutWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	scpClientWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationTests", mock.Anything).Return(timeoutWorkflowRun, nil)

	scpClientWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name           string
		reqOrgName     string
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		scpClient      *tmocks.Client
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClient,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			scpClient:      scpClient,
		},
		{
			name:           "error when workflow times out",
			reqOrgName:     ipOrg1,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClientWithTimeout,
		},
		{
			name:           "no error",
			reqOrgName:     ipOrg1,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// init temporal client
			scp.IDClientMap[site.ID.String()] = tc.scpClient
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "siteID")
			ec.SetParamValues(tc.reqOrgName, site.ID.String())
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := GetAllMachineValidationTestHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err := cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				var apiResponse []*model.APIMachineValidationTest
				err := json.Unmarshal(rec.Body.Bytes(), &apiResponse)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, len(workflowResponse), len(apiResponse))
				for i, expected := range workflowResponse {
					assert.Equal(t, expected.TestId, apiResponse[i].TestID)
					assert.Equal(t, expected.Version, apiResponse[i].Version)
				}
			}
		})
	}
}

func TestGetMachineValidationTestHandler(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipRoles := []string{authz.ProviderAdminRole}

	pvu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	// Prepare client pool for sync calls to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// init scp client
	scpClient := &tmocks.Client{}

	testID := "test-id-1"
	testVersion := "test-version-3"

	// set-up create workflow
	updateWorkflowRun := &tmocks.WorkflowRun{}
	updateWorkflowRun.On("GetID").Return("get-workflow-id")

	updateWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.MachineValidationTestsGetResponse)
			*response = &corev1.MachineValidationTestsGetResponse{
				Tests: []*corev1.MachineValidationTest{
					{
						TestId:  testID,
						Version: testVersion,
					},
				},
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationTests", mock.Anything).Return(updateWorkflowRun, nil)

	// scp client with timeout
	scpClientWithTimeout := &tmocks.Client{}

	timeoutWorkflowRun := &tmocks.WorkflowRun{}
	timeoutWorkflowRun.On("GetID").Return("get-workflow-id")

	timeoutWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	scpClientWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationTests", mock.Anything).Return(timeoutWorkflowRun, nil)

	scpClientWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name           string
		reqOrgName     string
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		scpClient      *tmocks.Client
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClient,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			scpClient:      scpClient,
		},
		{
			name:           "error when workflow times out",
			reqOrgName:     ipOrg1,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClientWithTimeout,
		},
		{
			name:           "no error",
			reqOrgName:     ipOrg1,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// init temporal client
			scp.IDClientMap[site.ID.String()] = tc.scpClient
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "siteID", "id", "version")
			ec.SetParamValues(tc.reqOrgName, site.ID.String(), testID, testVersion)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := GetMachineValidationTestHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err := cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				var apiResponse *model.APIMachineValidationTest
				err := json.Unmarshal(rec.Body.Bytes(), &apiResponse)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, testID, apiResponse.TestID)
				assert.Equal(t, testVersion, apiResponse.Version)
			}
		})
	}
}

func TestGetMachineValidationResultsHandler(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipRoles := []string{authz.ProviderAdminRole}

	pvu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, nil, false, false, cdbm.MachineStatusReady)

	otherIP := testMachineBuildInfrastructureProvider(t, dbSession, "test-ip-org-2", "infra-provider-2")
	otherSite := testMachineBuildSite(t, dbSession, otherIP, "test-site-2", cdbm.SiteStatusRegistered)
	otherMachine := testMachineBuildMachine(t, dbSession, otherIP.ID, otherSite.ID, nil, nil, false, false, cdbm.MachineStatusReady)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	// tests
	var workflowResponse []*corev1.MachineValidationResult
	for i := 0; i < 20; i++ {
		workflowResponse = append(workflowResponse, &corev1.MachineValidationResult{
			Name: fmt.Sprintf("test-result-%d", i),
		})
	}

	// Prepare client pool for sync calls to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// init scp client
	scpClient := &tmocks.Client{}

	// set-up create workflow
	updateWorkflowRun := &tmocks.WorkflowRun{}
	updateWorkflowRun.On("GetID").Return("get-results-workflow-id")

	updateWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.MachineValidationResultList)
			*response = &corev1.MachineValidationResultList{
				Results: workflowResponse,
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationResults", mock.Anything).Return(updateWorkflowRun, nil)

	// scp client with timeout
	scpClientWithTimeout := &tmocks.Client{}

	timeoutWorkflowRun := &tmocks.WorkflowRun{}
	timeoutWorkflowRun.On("GetID").Return("get-results-workflow-id")

	timeoutWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	scpClientWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationResults", mock.Anything).Return(timeoutWorkflowRun, nil)

	scpClientWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name           string
		reqOrgName     string
		machineID      string
		siteID         string
		legacyRoute    bool
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		scpClient      *tmocks.Client
		emptyResponse  bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			machineID:      machine.ID,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClient,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			machineID:      machine.ID,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			scpClient:      scpClient,
		},
		{
			name:           "error when machine is not found",
			reqOrgName:     ipOrg1,
			machineID:      uuid.NewString(),
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
			scpClient:      scpClient,
		},
		{
			name:           "error when machine belongs to another provider",
			reqOrgName:     ipOrg1,
			machineID:      otherMachine.ID,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
			scpClient:      scpClient,
		},
		{
			name:           "error when legacy site belongs to another provider",
			reqOrgName:     ipOrg1,
			machineID:      uuid.NewString(),
			siteID:         otherSite.ID.String(),
			legacyRoute:    true,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			scpClient:      scpClient,
		},
		{
			name:           "error when workflow times out",
			reqOrgName:     ipOrg1,
			machineID:      machine.ID,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClientWithTimeout,
		},
		{
			name:           "no error for machine route",
			reqOrgName:     ipOrg1,
			machineID:      machine.ID,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
		},
		{
			name:           "no error for legacy route without central machine record",
			reqOrgName:     ipOrg1,
			machineID:      uuid.NewString(),
			siteID:         site.ID.String(),
			legacyRoute:    true,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
		},
		{
			name:           "empty response is an array for machine route",
			reqOrgName:     ipOrg1,
			machineID:      machine.ID,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
			emptyResponse:  true,
		},
		{
			name:           "empty response is an array for legacy route",
			reqOrgName:     ipOrg1,
			machineID:      uuid.NewString(),
			siteID:         site.ID.String(),
			legacyRoute:    true,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
			emptyResponse:  true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if tc.emptyResponse {
				previousResponse := workflowResponse
				workflowResponse = nil
				defer func() { workflowResponse = previousResponse }()
			}
			assert.NotEqual(t, tc.name, "")
			// init temporal client
			scp.IDClientMap[site.ID.String()] = tc.scpClient
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			if tc.legacyRoute {
				ec.SetParamNames("orgName", "siteID", "machineID")
				ec.SetParamValues(tc.reqOrgName, tc.siteID, tc.machineID)
			} else {
				ec.SetParamNames("orgName", "id")
				ec.SetParamValues(tc.reqOrgName, tc.machineID)
			}
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := GetMachineValidationResultsHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err := cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				if tc.emptyResponse {
					assert.JSONEq(t, "[]", rec.Body.String())
				}
				var apiResponse []*model.APIMachineValidationResult
				err := json.Unmarshal(rec.Body.Bytes(), &apiResponse)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, len(workflowResponse), len(apiResponse))
				for i, expected := range workflowResponse {
					assert.Equal(t, expected.Name, apiResponse[i].Name)
				}
			}
		})
	}
}

func TestGetAllMachineValidationRunHandler(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipRoles := []string{authz.ProviderAdminRole}

	pvu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, nil, false, false, cdbm.MachineStatusReady)

	otherIP := testMachineBuildInfrastructureProvider(t, dbSession, "test-ip-org-2", "infra-provider-2")
	otherSite := testMachineBuildSite(t, dbSession, otherIP, "test-site-2", cdbm.SiteStatusRegistered)
	otherMachine := testMachineBuildMachine(t, dbSession, otherIP.ID, otherSite.ID, nil, nil, false, false, cdbm.MachineStatusReady)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	// tests
	var workflowResponse []*corev1.MachineValidationRun
	for i := 0; i < 20; i++ {
		workflowResponse = append(workflowResponse, &corev1.MachineValidationRun{
			Name: fmt.Sprintf("test-run-%d", i),
		})
	}

	// Prepare client pool for sync calls to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// init scp client
	scpClient := &tmocks.Client{}

	// set-up create workflow
	updateWorkflowRun := &tmocks.WorkflowRun{}
	updateWorkflowRun.On("GetID").Return("get-runs-workflow-id")

	updateWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.MachineValidationRunList)
			*response = &corev1.MachineValidationRunList{
				Runs: workflowResponse,
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationRuns", mock.Anything).Return(updateWorkflowRun, nil)

	// scp client with timeout
	scpClientWithTimeout := &tmocks.Client{}

	timeoutWorkflowRun := &tmocks.WorkflowRun{}
	timeoutWorkflowRun.On("GetID").Return("get-runs-workflow-id")

	timeoutWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	scpClientWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationRuns", mock.Anything).Return(timeoutWorkflowRun, nil)

	scpClientWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name           string
		reqOrgName     string
		machineID      string
		siteID         string
		legacyRoute    bool
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		scpClient      *tmocks.Client
		emptyResponse  bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			machineID:      machine.ID,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClient,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			machineID:      machine.ID,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			scpClient:      scpClient,
		},
		{
			name:           "error when machine is not found",
			reqOrgName:     ipOrg1,
			machineID:      uuid.NewString(),
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
			scpClient:      scpClient,
		},
		{
			name:           "error when machine belongs to another provider",
			reqOrgName:     ipOrg1,
			machineID:      otherMachine.ID,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
			scpClient:      scpClient,
		},
		{
			name:           "error when legacy site belongs to another provider",
			reqOrgName:     ipOrg1,
			machineID:      uuid.NewString(),
			siteID:         otherSite.ID.String(),
			legacyRoute:    true,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			scpClient:      scpClient,
		},
		{
			name:           "error when workflow times out",
			reqOrgName:     ipOrg1,
			machineID:      machine.ID,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClientWithTimeout,
		},
		{
			name:           "no error for machine route",
			reqOrgName:     ipOrg1,
			machineID:      machine.ID,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
		},
		{
			name:           "no error for legacy route without central machine record",
			reqOrgName:     ipOrg1,
			machineID:      uuid.NewString(),
			siteID:         site.ID.String(),
			legacyRoute:    true,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
		},
		{
			name:           "empty response is an array for machine route",
			reqOrgName:     ipOrg1,
			machineID:      machine.ID,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
			emptyResponse:  true,
		},
		{
			name:           "empty response is an array for legacy route",
			reqOrgName:     ipOrg1,
			machineID:      uuid.NewString(),
			siteID:         site.ID.String(),
			legacyRoute:    true,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
			emptyResponse:  true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if tc.emptyResponse {
				previousResponse := workflowResponse
				workflowResponse = nil
				defer func() { workflowResponse = previousResponse }()
			}
			assert.NotEqual(t, tc.name, "")
			// init temporal client
			scp.IDClientMap[site.ID.String()] = tc.scpClient
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			if tc.legacyRoute {
				ec.SetParamNames("orgName", "siteID", "machineID")
				ec.SetParamValues(tc.reqOrgName, tc.siteID, tc.machineID)
			} else {
				ec.SetParamNames("orgName", "id")
				ec.SetParamValues(tc.reqOrgName, tc.machineID)
			}
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := GetAllMachineValidationRunHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err := cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				if tc.emptyResponse {
					assert.JSONEq(t, "[]", rec.Body.String())
				}
				var apiResponse []*model.APIMachineValidationRun
				err := json.Unmarshal(rec.Body.Bytes(), &apiResponse)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, len(workflowResponse), len(apiResponse))
				for i, expected := range workflowResponse {
					assert.Equal(t, expected.Name, apiResponse[i].Name)
				}
			}
		})
	}
}

func TestGetAllMachineValidationExternalConfigHandler(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipRoles := []string{authz.ProviderAdminRole}

	pvu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	// tests
	var workflowResponse []*corev1.MachineValidationExternalConfig
	for i := 0; i < 20; i++ {
		workflowResponse = append(workflowResponse, &corev1.MachineValidationExternalConfig{
			Name: fmt.Sprintf("test-ext-cfg-%d", i),
		})
	}

	// Prepare client pool for sync calls to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// init scp client
	scpClient := &tmocks.Client{}

	// set-up get workflow
	getWorkflowRun := &tmocks.WorkflowRun{}
	getWorkflowRun.On("GetID").Return("get-all-ext-cfg-workflow-id")

	getWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.GetMachineValidationExternalConfigsResponse)
			*response = &corev1.GetMachineValidationExternalConfigsResponse{
				Configs: workflowResponse,
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationExternalConfigs", mock.Anything).Return(getWorkflowRun, nil)

	// scp client with timeout
	scpClientWithTimeout := &tmocks.Client{}

	timeoutWorkflowRun := &tmocks.WorkflowRun{}
	timeoutWorkflowRun.On("GetID").Return("get-all-ext-cfg-workflow-id")

	timeoutWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	scpClientWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationExternalConfigs", mock.Anything).Return(timeoutWorkflowRun, nil)

	scpClientWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name           string
		reqOrgName     string
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		scpClient      *tmocks.Client
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClient,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			scpClient:      scpClient,
		},
		{
			name:           "error when workflow times out",
			reqOrgName:     ipOrg1,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClientWithTimeout,
		},
		{
			name:           "no error",
			reqOrgName:     ipOrg1,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// init temporal client
			scp.IDClientMap[site.ID.String()] = tc.scpClient
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "siteID")
			ec.SetParamValues(tc.reqOrgName, site.ID.String())
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := GetAllMachineValidationExternalConfigHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err := cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				var apiResponse []*model.APIMachineValidationExternalConfig
				err := json.Unmarshal(rec.Body.Bytes(), &apiResponse)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, len(workflowResponse), len(apiResponse))
				for i, expected := range workflowResponse {
					assert.Equal(t, expected.Name, apiResponse[i].Name)
				}
			}
		})
	}
}

func TestGetMachineValidationExternalConfigHandler(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipRoles := []string{authz.ProviderAdminRole}

	pvu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	expCfgName := "test-ext-cfg-13"
	// tests
	var workflowResponse []*corev1.MachineValidationExternalConfig
	workflowResponse = append(workflowResponse, &corev1.MachineValidationExternalConfig{
		Name: expCfgName,
	})

	// Prepare client pool for sync calls to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// init scp client
	scpClient := &tmocks.Client{}

	// set-up get workflow
	getWorkflowRun := &tmocks.WorkflowRun{}
	getWorkflowRun.On("GetID").Return("get-ext-cfg-workflow-id")

	getWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.GetMachineValidationExternalConfigsResponse)
			*response = &corev1.GetMachineValidationExternalConfigsResponse{
				Configs: workflowResponse,
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationExternalConfigs", mock.Anything).Return(getWorkflowRun, nil)

	// scp client with empty response
	scpEmptyClient := &tmocks.Client{}

	// set-up get workflow
	emptyWorkflowRun := &tmocks.WorkflowRun{}
	emptyWorkflowRun.On("GetID").Return("get-ext-cfg-workflow-id")

	emptyWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.GetMachineValidationExternalConfigsResponse)
			*response = &corev1.GetMachineValidationExternalConfigsResponse{}
			return nil
		},
	)

	scpEmptyClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationExternalConfigs", mock.Anything).Return(emptyWorkflowRun, nil)

	// scp client with timeout
	scpClientWithTimeout := &tmocks.Client{}

	timeoutWorkflowRun := &tmocks.WorkflowRun{}
	timeoutWorkflowRun.On("GetID").Return("get-ext-cfg-workflow-id")

	timeoutWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	scpClientWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationExternalConfigs", mock.Anything).Return(timeoutWorkflowRun, nil)

	scpClientWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name           string
		reqOrgName     string
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		scpClient      *tmocks.Client
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClient,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			scpClient:      scpClient,
		},
		{
			name:           "error when workflow times out",
			reqOrgName:     ipOrg1,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClientWithTimeout,
		},
		{
			name:           "error when no config returned",
			reqOrgName:     ipOrg1,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
			scpClient:      scpEmptyClient,
		},
		{
			name:           "no error",
			reqOrgName:     ipOrg1,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// init temporal client
			scp.IDClientMap[site.ID.String()] = tc.scpClient
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "siteID", "cfgName")
			ec.SetParamValues(tc.reqOrgName, site.ID.String(), expCfgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := GetMachineValidationExternalConfigHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err := cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				var apiResponse *model.APIMachineValidationExternalConfig
				err := json.Unmarshal(rec.Body.Bytes(), &apiResponse)
				assert.Nil(t, err)
				assert.NotNil(t, apiResponse)
				assert.Equal(t, expCfgName, apiResponse.Name)
			}
		})
	}
}

func TestCreateMachineValidationExternalConfigHandler(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipRoles := []string{authz.ProviderAdminRole}

	pvu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	// identity
	extCfgName := "ext-cfg-1"
	extCfgRaw := []byte{0, 12, 34, 53, 12}

	// Prepare client pool for sync calls to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// init scp client
	scpClient := &tmocks.Client{}

	// set-up create workflow
	createWorkflowRun := &tmocks.WorkflowRun{}
	createWorkflowRun.On("GetID").Return("create-workflow-id")

	createWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"AddUpdateMachineValidationExternalConfig", mock.Anything).Return(createWorkflowRun, nil)

	// set-up get workflow
	getWorkflowID := "get-workflow-id"
	getWorkflowRun := &tmocks.WorkflowRun{}
	getWorkflowRun.On("GetID").Return(getWorkflowID)

	getWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			response := value.(**corev1.GetMachineValidationExternalConfigsResponse)
			*response = &corev1.GetMachineValidationExternalConfigsResponse{
				Configs: []*corev1.MachineValidationExternalConfig{
					{
						Name:   extCfgName,
						Config: extCfgRaw,
					},
				},
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationExternalConfigs", mock.Anything).Return(getWorkflowRun, nil)

	// scp client with timeout
	scpClientWithTimeout := &tmocks.Client{}

	timeoutWorkflowRun := &tmocks.WorkflowRun{}
	timeoutWorkflowRun.On("GetID").Return("create-workflow-id")

	timeoutWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	scpClientWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"AddUpdateMachineValidationExternalConfig", mock.Anything).Return(timeoutWorkflowRun, nil)

	scpClientWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name           string
		reqOrgName     string
		reqBodyModel   *model.APIMachineValidationExternalConfigCreateRequest
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		scpClient      *tmocks.Client
	}{
		{
			name:       "error when user not found in request context",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationExternalConfigCreateRequest{
				Name:   extCfgName,
				Config: extCfgRaw,
			},
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClient,
		},
		{
			name:       "error when user not found in org",
			reqOrgName: "SomeOrg",
			reqBodyModel: &model.APIMachineValidationExternalConfigCreateRequest{
				Name:   extCfgName,
				Config: extCfgRaw,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			scpClient:      scpClient,
		},
		{
			name:       "error when request does not include Name",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationExternalConfigCreateRequest{
				Config: extCfgRaw,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			scpClient:      scpClient,
		},
		{
			name:       "error when request does not include Config",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationExternalConfigCreateRequest{
				Name: extCfgName,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			scpClient:      scpClient,
		},
		{
			name:       "error when workflow times out",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationExternalConfigCreateRequest{
				Name:   extCfgName,
				Config: extCfgRaw,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClientWithTimeout,
		},
		{
			name:       "no error",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationExternalConfigCreateRequest{
				Name:   extCfgName,
				Config: extCfgRaw,
			},
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			scpClient:      scpClient,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// init temporal client
			scp.IDClientMap[site.ID.String()] = tc.scpClient
			// prep body
			jsonBody, err := json.Marshal(tc.reqBodyModel)
			assert.Nil(t, err)
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(jsonBody)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "siteID")
			ec.SetParamValues(tc.reqOrgName, site.ID.String())
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := CreateMachineValidationExternalConfigHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err = cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusCreated)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				apiResponse := &model.APIMachineValidationExternalConfig{}
				err := json.Unmarshal(rec.Body.Bytes(), apiResponse)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, extCfgName, apiResponse.Name)
			}
		})
	}
}

func TestUpdateMachineValidationExternalConfigHandler(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipRoles := []string{authz.ProviderAdminRole}

	pvu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	// test identity
	extCfgName := "ext-cfg-1"
	extCfgRaw := []byte{0, 12, 34, 53, 12}
	extCfgDescription := "test description"

	// Prepare client pool for sync calls to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// init scp client
	scpClient := &tmocks.Client{}

	// set-up create workflow
	updateWorkflowRun := &tmocks.WorkflowRun{}
	updateWorkflowRun.On("GetID").Return("update-workflow-id")

	updateWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"AddUpdateMachineValidationExternalConfig", mock.Anything).Return(updateWorkflowRun, nil)

	// set-up get workflow
	beforeUpdate := true
	getWorkflowID := "get-workflow-id"
	getWorkflowRun := &tmocks.WorkflowRun{}
	getWorkflowRun.On("GetID").Return(getWorkflowID)

	getWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(
		func(ctx context.Context, value interface{}) error {
			if beforeUpdate {
				response := value.(**corev1.GetMachineValidationExternalConfigsResponse)
				*response = &corev1.GetMachineValidationExternalConfigsResponse{
					Configs: []*corev1.MachineValidationExternalConfig{
						{
							Name:   extCfgName,
							Config: extCfgRaw,
						},
					},
				}
				beforeUpdate = false
			} else {
				response := value.(**corev1.GetMachineValidationExternalConfigsResponse)
				*response = &corev1.GetMachineValidationExternalConfigsResponse{
					Configs: []*corev1.MachineValidationExternalConfig{
						{
							Name:        extCfgName,
							Config:      extCfgRaw,
							Description: &extCfgDescription,
						},
					},
				}
			}
			return nil
		},
	)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"GetMachineValidationExternalConfigs", mock.Anything).Return(getWorkflowRun, nil)

	tests := []struct {
		name           string
		reqOrgName     string
		reqBodyModel   *model.APIMachineValidationExternalConfigUpdateRequest
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		scpClient      *tmocks.Client
	}{
		{
			name:       "error when user not found in request context",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationExternalConfigUpdateRequest{
				Description: &extCfgDescription,
			},
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClient,
		},
		{
			name:       "error when user not found in org",
			reqOrgName: "SomeOrg",
			reqBodyModel: &model.APIMachineValidationExternalConfigUpdateRequest{
				Description: &extCfgDescription,
			},
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			scpClient:      scpClient,
		},
		{
			name:       "no error",
			reqOrgName: ipOrg1,
			reqBodyModel: &model.APIMachineValidationExternalConfigUpdateRequest{
				Description: &extCfgDescription,
			},
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			scpClient:      scpClient,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// init temporal client
			scp.IDClientMap[site.ID.String()] = tc.scpClient
			// prep body
			jsonBody, err := json.Marshal(tc.reqBodyModel)
			assert.Nil(t, err)
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(string(jsonBody)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			// re-set before update flag
			beforeUpdate = true

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "siteID", "cfgName")
			ec.SetParamValues(tc.reqOrgName, site.ID.String(), extCfgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := UpdateMachineValidationExternalConfigHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err = cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				apiResponse := &model.APIMachineValidationExternalConfig{}
				err := json.Unmarshal(rec.Body.Bytes(), apiResponse)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, extCfgName, apiResponse.Name)
				assert.Equal(t, extCfgDescription, apiResponse.Description)
			}
		})
	}
}

func TestDeleteMachineValidationExternalConfigHandler(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipRoles := []string{authz.ProviderAdminRole}

	pvu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	// identity
	extCfgName := "ext-cfg-1"

	// Prepare client pool for sync calls to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// init scp client
	scpClient := &tmocks.Client{}

	// set-up delete workflow
	deleteWorkflowRun := &tmocks.WorkflowRun{}
	deleteWorkflowRun.On("GetID").Return("delete-workflow-id")

	deleteWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	scpClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"RemoveMachineValidationExternalConfig", mock.Anything).Return(deleteWorkflowRun, nil)

	// scp client with timeout
	scpClientWithTimeout := &tmocks.Client{}

	timeoutWorkflowRun := &tmocks.WorkflowRun{}
	timeoutWorkflowRun.On("GetID").Return("delete-workflow-id")

	timeoutWorkflowRun.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	scpClientWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"RemoveMachineValidationExternalConfig", mock.Anything).Return(timeoutWorkflowRun, nil)

	scpClientWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name           string
		reqOrgName     string
		user           *cdbm.User
		expectedErr    bool
		expectedStatus int
		scpClient      *tmocks.Client
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClient,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			scpClient:      scpClient,
		},
		{
			name:           "error when workflow times out",
			reqOrgName:     ipOrg1,
			user:           pvu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			scpClient:      scpClientWithTimeout,
		},
		{
			name:           "no error",
			reqOrgName:     ipOrg1,
			user:           pvu,
			expectedErr:    false,
			expectedStatus: http.StatusAccepted,
			scpClient:      scpClient,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// init temporal client
			scp.IDClientMap[site.ID.String()] = tc.scpClient
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodDelete, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "siteID", "cfgName")
			ec.SetParamValues(tc.reqOrgName, site.ID.String(), extCfgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := DeleteMachineValidationExternalConfigHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err := cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusAccepted)
			assert.Equal(t, tc.expectedStatus, rec.Code)
		})
	}
}
