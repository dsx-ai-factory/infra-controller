// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strconv"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalEnums "go.temporal.io/api/enums/v1"
	tClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestGetTaskHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	siteNoRLA := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-site-no-flow",
		Org:                      org,
		InfrastructureProviderID: site.InfrastructureProviderID,
		Status:                   cdbm.SiteStatusRegistered,
		Config:                   &cdbm.SiteConfig{},
	}
	_, err := dbSession.DB.NewInsert().Model(siteNoRLA).Exec(context.Background())
	assert.Nil(t, err)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-task-get", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-task-get", org, []string{authz.TenantAdminRole})

	handler := NewGetTaskHandler(dbSession, nil, scp, cfg)

	taskUUID := uuid.New().String()

	mockTask := &flowv1.Task{
		Id:          &flowv1.UUID{Id: taskUUID},
		Operation:   "power_on",
		RackId:      &flowv1.UUID{Id: uuid.New().String()},
		Description: "Power on rack",
		Status:      flowv1.TaskStatus_TASK_STATUS_COMPLETED,
		Message:     "Processing",
		Report:      `{"version":1,"stages":[{"number":1,"status":"completed","steps":[{"component_type":"Compute","status":"skipped"}]}]}`,
	}

	failedTask := proto.Clone(mockTask).(*flowv1.Task)
	failedTask.Status = flowv1.TaskStatus_TASK_STATUS_FAILED

	failureConverter := tp.GetDefaultFailureConverter()
	missingTaskError := failureConverter.FailureToError(failureConverter.ErrorToFailure(
		swe.WrapErr(status.Error(codes.NotFound, "task not found")),
	))

	ctx := context.Background()

	tests := []struct {
		name               string
		reqOrg             string
		user               *cdbm.User
		taskUUID           string
		queryParams        map[string]string
		mockTasks          []*flowv1.Task
		mockResultErr      error
		expectedStatus     int
		expectedTaskStatus string
	}{
		{
			name:     "success - get task by ID",
			reqOrg:   org,
			user:     providerUser,
			taskUUID: taskUUID,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			mockTasks:          []*flowv1.Task{mockTask},
			expectedStatus:     http.StatusOK,
			expectedTaskStatus: "Succeeded",
		},
		{
			name:               "success - failed task remains readable",
			reqOrg:             org,
			user:               providerUser,
			taskUUID:           taskUUID,
			queryParams:        map[string]string{"siteId": site.ID.String()},
			mockTasks:          []*flowv1.Task{failedTask},
			expectedStatus:     http.StatusOK,
			expectedTaskStatus: "Failed",
		},
		{
			name:     "failure - task not found (empty result)",
			reqOrg:   org,
			user:     providerUser,
			taskUUID: taskUUID,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			mockTasks:      []*flowv1.Task{},
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "failure - missing task from Flow",
			reqOrg:         org,
			user:           providerUser,
			taskUUID:       taskUUID,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			mockResultErr:  missingTaskError,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "failure - database error from Flow",
			reqOrg:         org,
			user:           providerUser,
			taskUUID:       taskUUID,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			mockResultErr:  tp.NewApplicationError(status.Error(codes.Internal, "database unavailable").Error(), "Error"),
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:     "failure - Flow not enabled on site",
			reqOrg:   org,
			user:     providerUser,
			taskUUID: taskUUID,
			queryParams: map[string]string{
				"siteId": siteNoRLA.ID.String(),
			},
			expectedStatus: http.StatusPreconditionFailed,
		},
		{
			name:        "failure - missing siteId",
			reqOrg:      org,
			user:        providerUser,
			taskUUID:    taskUUID,
			queryParams: map[string]string{
				// no siteId
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:     "failure - invalid siteId",
			reqOrg:   org,
			user:     providerUser,
			taskUUID: taskUUID,
			queryParams: map[string]string{
				"siteId": uuid.New().String(),
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:     "failure - tenant access denied",
			reqOrg:   org,
			user:     tenantUser,
			taskUUID: taskUUID,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			expectedStatus: http.StatusForbidden,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			if tt.mockTasks != nil {
				testFlowProxyReply(t, mockWorkflowRun, &flowv1.GetTasksByIDsResponse{Tasks: tt.mockTasks})
			} else if tt.mockResultErr != nil {
				mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Return(tt.mockResultErr)
			}
			testFlowProxyDispatch(t, mockTemporalClient, mockWorkflowRun, flowv1.Flow_GetTasksByIDs_FullMethodName, nil)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			q := url.Values{}
			for k, v := range tt.queryParams {
				q.Set(k, v)
			}
			path := fmt.Sprintf("/v2/org/%s/nico/rack/task/%s?%s", tt.reqOrg, tt.taskUUID, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrg, tt.taskUUID)
			ec.Set("user", tt.user)

			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)

			if tt.expectedStatus != rec.Code {
				t.Errorf("GetTaskHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.mockResultErr != nil {
				expectedError := tt.mockResultErr
				if tt.expectedStatus == http.StatusNotFound {
					expectedError = errors.New("Task not found")
					var appErr *tp.ApplicationError
					require.ErrorAs(t, tt.mockResultErr, &appErr)
					require.True(t, appErr.NonRetryable())
					require.Contains(t, rec.Body.String(), "Task not found")
					require.NotContains(t, rec.Body.String(), "sql:")
					require.NotContains(t, rec.Body.String(), "retryable: true")
				}
				assertSingleFlowProxyErrorResponse(t, ec, rec, err, expectedError)
			}
			if tt.expectedStatus != http.StatusOK {
				return
			}

			var apiTask model.APITask
			err = json.Unmarshal(rec.Body.Bytes(), &apiTask)
			assert.NoError(t, err)
			assert.Equal(t, taskUUID, apiTask.ID)
			assert.Equal(t, tt.expectedTaskStatus, apiTask.Status)
			assert.Equal(t, "Power on rack", apiTask.Description)
			assert.Equal(t, "Processing", apiTask.Message)
			assertZeroTaskReportCounters(t, rec.Body.Bytes())
		})
	}
}

// ExecuteGetTasksHandlerTestCases exercises the root, Rack, and Tray task-list
// handlers with a shared case matrix. pathFmt and the path parameter differ per
// handler; all invoke Flow ListTasks through the generic proxy.
type GetTasksHandlerTestCase struct {
	name              string
	reqOrg            string
	user              *cdbm.User
	pathParam         string
	queryParams       map[string]string
	mockTasks         []*flowv1.Task
	mockTotal         *int32
	expectedStatus    int
	expectedPage      *pagination.PageResponse
	appliedFilters    *flowv1.TaskListAppliedFilters
	proxyErr          error
	assertFlowReq     func(t *testing.T, req *flowv1.ListTasksRequest, pathParam string)
	assertResponse    func(t *testing.T, tasks []model.APITask)
	assertRawResponse func(t *testing.T, body []byte)
}

func ExecuteGetTasksHandlerTestCases(t *testing.T, pathFmt string, handle func(echo.Context) error, scp *sc.ClientPool, siteID string, testCases []GetTasksHandlerTestCase) {
	t.Helper()
	e := echo.New()
	for _, tt := range testCases {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			if tt.mockTasks != nil {
				total := int32(len(tt.mockTasks))
				if tt.mockTotal != nil {
					total = *tt.mockTotal
				}
				testFlowProxyReply(t, mockWorkflowRun, &flowv1.ListTasksResponse{
					Tasks:          tt.mockTasks,
					Total:          total,
					AppliedFilters: tt.appliedFilters,
				})
			}
			if tt.proxyErr != nil {
				mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Return(tt.proxyErr)
			}
			mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Flow.WorkflowName, mock.Anything).
				Run(func(args mock.Arguments) {
					assert.Equal(t, flowv1.Flow_ListTasks_FullMethodName, args.Get(3).(grpcproxy.Request).FullMethod)
					query := url.Values{}
					for name, value := range tt.queryParams {
						switch name {
						case "activeOnly", "includeReport", "eventTriggeredOnly":
							enabled, parseErr := strconv.ParseBool(value)
							require.NoError(t, parseErr)
							if !enabled {
								continue
							}
							value = "true"
						}
						query.Set(name, value)
					}
					if query.Get("pageNumber") == "" {
						query.Set("pageNumber", "1")
					}
					if query.Get("pageSize") == "" {
						query.Set("pageSize", "20")
					}
					hash := common.QueryParamHash(query)
					expectedID := "task-get-all-" + hash
					switch pathFmt {
					case "/v2/org/%s/nico/rack/%s/task":
						expectedID = "tasks-rack-get-" + tt.pathParam + "-" + hash
					case "/v2/org/%s/nico/tray/%s/task":
						expectedID = "tasks-tray-get-" + tt.pathParam + "-" + hash
					}
					options := args.Get(1).(tClient.StartWorkflowOptions)
					assert.Equal(t, expectedID, options.ID)
					assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, options.WorkflowIDConflictPolicy)
					if tt.assertFlowReq != nil {
						req := &flowv1.ListTasksRequest{}
						testFlowProxyRequest(t, args, req)
						tt.assertFlowReq(t, req, tt.pathParam)
					}
				}).
				Return(mockWorkflowRun, nil)
			scp.IDClientMap[siteID] = mockTemporalClient

			q := url.Values{}
			for k, v := range tt.queryParams {
				q.Set(k, v)
			}
			var path string
			if tt.pathParam == "" {
				path = fmt.Sprintf(pathFmt, tt.reqOrg)
			} else {
				path = fmt.Sprintf(pathFmt, tt.reqOrg, tt.pathParam)
			}
			path += "?" + q.Encode()
			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.reqOrg)
			if tt.pathParam != "" {
				ec.SetParamNames("orgName", "id")
				ec.SetParamValues(tt.reqOrg, tt.pathParam)
			}
			ec.Set("user", tt.user)

			err := handle(ec)
			require.Equal(t, tt.expectedStatus, rec.Code, "body=%s err=%v", rec.Body.String(), err)

			if tt.mockTasks == nil && tt.proxyErr == nil {
				mockTemporalClient.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
			} else {
				mockTemporalClient.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			}
			if tt.expectedStatus != http.StatusOK {
				assert.NotContains(t, rec.Body.String(), "Power on rack")
				assert.Empty(t, rec.Header().Get(pagination.ResponseHeaderName))
				return
			}
			var tasks []model.APITask
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &tasks))
			require.Len(t, tasks, len(tt.mockTasks))
			pageHeader := rec.Header().Get(pagination.ResponseHeaderName)
			require.NotEmpty(t, pageHeader, pagination.ResponseHeaderName)
			if tt.expectedPage != nil {
				var pageResponse pagination.PageResponse
				require.NoError(t, json.Unmarshal([]byte(pageHeader), &pageResponse))
				assert.Equal(t, *tt.expectedPage, pageResponse)
			}
			if tt.assertResponse != nil {
				tt.assertResponse(t, tasks)
			}
			if tt.assertRawResponse != nil {
				tt.assertRawResponse(t, rec.Body.Bytes())
			}
		})
	}
}

func assertZeroTaskReportCounters(t *testing.T, body []byte) {
	t.Helper()
	for _, field := range []string{"totalComponents", "succeededComponents", "failedComponents"} {
		assert.Contains(t, string(body), `"`+field+`":0`)
	}
	assert.NotContains(t, string(body), `"completedComponents"`)
}

func TestGetAllTaskHandler_Handle(t *testing.T) {
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)
	siteWithoutFlow := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-site-task-list-no-flow",
		Org:                      org,
		InfrastructureProviderID: site.InfrastructureProviderID,
		Status:                   cdbm.SiteStatusRegistered,
		Config:                   &cdbm.SiteConfig{},
	}
	_, err := dbSession.DB.NewInsert().Model(siteWithoutFlow).Exec(context.Background())
	require.NoError(t, err)

	foreignProvider := &cdbm.InfrastructureProvider{ID: uuid.New(), Name: "other Task Provider", Org: "other-task-provider"}
	_, err = dbSession.DB.NewInsert().Model(foreignProvider).Exec(t.Context())
	require.NoError(t, err)
	foreignSite := *site
	foreignSite.ID = uuid.New()
	foreignSite.Name = "foreign-task-site"
	foreignSite.Org = foreignProvider.Org
	foreignSite.InfrastructureProviderID = foreignProvider.ID
	_, err = dbSession.DB.NewInsert().Model(&foreignSite).Exec(t.Context())
	require.NoError(t, err)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-task-list-site", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-task-list-site", org, []string{authz.TenantAdminRole})

	handler := NewGetAllTaskHandler(dbSession, scp)
	taskUUID := uuid.New().String()
	listed := []*flowv1.Task{{
		Id:          &flowv1.UUID{Id: taskUUID},
		RackId:      &flowv1.UUID{Id: uuid.New().String()},
		Description: "Power on rack",
		Status:      flowv1.TaskStatus_TASK_STATUS_RUNNING,
		Report:      `{"version":1,"stages":[{"number":1,"status":"completed","steps":[{"component_type":"Compute","status":"skipped"}]}]}`,
	}}

	cases := []GetTasksHandlerTestCase{
		{
			name:           "success - list every task in site",
			reqOrg:         org,
			user:           providerUser,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			mockTasks:      listed,
			expectedStatus: http.StatusOK,
			expectedPage:   &pagination.PageResponse{PageNumber: 1, PageSize: 20, Total: 1},
			assertFlowReq: func(t *testing.T, req *flowv1.ListTasksRequest, _ string) {
				t.Helper()
				assert.Nil(t, req.GetRackId())
				assert.Nil(t, req.GetComponentId())
				assert.False(t, req.GetActiveOnly())
				assert.False(t, req.GetWithReport())
			},
			assertResponse: func(t *testing.T, tasks []model.APITask) {
				t.Helper()
				assert.Nil(t, tasks[0].Report)
			},
		},
		{
			name:              "success - filters and pagination pass through",
			assertRawResponse: assertZeroTaskReportCounters,
			reqOrg:            org,
			user:              providerUser,
			queryParams:       map[string]string{"siteId": site.ID.String(), "activeOnly": "true", "includeReport": "true", "pageNumber": "2", "pageSize": "10"},
			mockTasks:         listed,
			expectedStatus:    http.StatusOK,
			expectedPage:      &pagination.PageResponse{PageNumber: 2, PageSize: 10, Total: 1},
			assertFlowReq: func(t *testing.T, req *flowv1.ListTasksRequest, _ string) {
				t.Helper()
				assert.True(t, req.GetActiveOnly())
				assert.True(t, req.GetWithReport())
				require.NotNil(t, req.GetPagination())
				assert.Equal(t, int32(10), req.GetPagination().GetOffset())
				assert.Equal(t, int32(10), req.GetPagination().GetLimit())
			},
			assertResponse: func(t *testing.T, tasks []model.APITask) {
				t.Helper()
				require.NotNil(t, tasks[0].Report)
				assert.Equal(t, 1, tasks[0].Report.Version)
			},
		},
		{
			name:           "failure - invalid site UUID",
			reqOrg:         org,
			user:           providerUser,
			queryParams:    map[string]string{"siteId": "not-a-uuid"},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - site does not exist",
			reqOrg:         org,
			user:           providerUser,
			queryParams:    map[string]string{"siteId": uuid.New().String()},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - Flow not enabled on site",
			reqOrg:         org,
			user:           providerUser,
			queryParams:    map[string]string{"siteId": siteWithoutFlow.ID.String()},
			expectedStatus: http.StatusPreconditionFailed,
		},
		{
			name:           "failure - tenant access denied",
			reqOrg:         org,
			user:           tenantUser,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "failure - authorization precedes query validation",
			reqOrg:         org,
			user:           tenantUser,
			queryParams:    map[string]string{"unknown": "value"},
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "failure - unknown query parameter",
			reqOrg:         org,
			user:           providerUser,
			queryParams:    map[string]string{"siteId": site.ID.String(), "unknown": "value"},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			queryParams:    map[string]string{},
			expectedStatus: http.StatusBadRequest,
		},
	}

	policyID := uuid.NewString()
	cases = append(cases,
		GetTasksHandlerTestCase{name: "missing user", reqOrg: org, queryParams: map[string]string{"siteId": site.ID.String()}, expectedStatus: http.StatusInternalServerError},
		GetTasksHandlerTestCase{name: "org membership", reqOrg: "unrelated-org", user: providerUser, queryParams: map[string]string{"siteId": site.ID.String()}, expectedStatus: http.StatusForbidden},
		GetTasksHandlerTestCase{name: "Provider Admin cannot access foreign Site", reqOrg: org, user: providerUser, queryParams: map[string]string{"siteId": foreignSite.ID.String()}, expectedStatus: http.StatusForbidden},

		GetTasksHandlerTestCase{
			name: "event provenance report and page selectors", reqOrg: org, user: providerUser,
			queryParams: map[string]string{"siteId": site.ID.String(), "eventType": "hardware.leak.detected", "eventPolicyId": policyID, "eventTriggeredOnly": "false", "activeOnly": "true", "includeReport": "true", "pageNumber": "2", "pageSize": "10"},
			mockTasks:   listed, expectedStatus: http.StatusOK,
			expectedPage:   &pagination.PageResponse{PageNumber: 2, PageSize: 10, Total: 1},
			appliedFilters: &flowv1.TaskListAppliedFilters{EventFilter: &flowv1.EventTaskFilter{EventType: cutil.GetPtr("hardware.leak.detected"), PolicyId: &flowv1.UUID{Id: policyID}}},
			assertFlowReq: func(t *testing.T, req *flowv1.ListTasksRequest, _ string) {
				assert.Nil(t, req.GetRackId())
				assert.Equal(t, policyID, req.GetEventFilter().GetPolicyId().GetId())
				assert.Equal(t, "hardware.leak.detected", req.GetEventFilter().GetEventType())
				assert.True(t, req.GetActiveOnly())
				assert.True(t, req.GetWithReport())
				assert.EqualValues(t, 10, req.GetPagination().GetOffset())
			},
			assertResponse: func(t *testing.T, tasks []model.APITask) { require.NotNil(t, tasks[0].Report) },
		},
		GetTasksHandlerTestCase{
			name: "all event Tasks including empty page", reqOrg: org, user: providerUser,
			queryParams: map[string]string{"siteId": site.ID.String(), "eventTriggeredOnly": "true"},
			mockTasks:   []*flowv1.Task{}, expectedStatus: http.StatusOK,
			appliedFilters: &flowv1.TaskListAppliedFilters{EventFilter: &flowv1.EventTaskFilter{}},
			expectedPage:   &pagination.PageResponse{PageNumber: 1, PageSize: 20, Total: 0},
			assertFlowReq: func(t *testing.T, req *flowv1.ListTasksRequest, _ string) {
				require.NotNil(t, req.EventFilter)
				assert.Nil(t, req.RackId)
			},
		},
		GetTasksHandlerTestCase{
			name: "older service ignores event filter", reqOrg: org, user: providerUser,
			queryParams: map[string]string{"siteId": site.ID.String(), "eventTriggeredOnly": "true"},
			mockTasks:   listed, expectedStatus: http.StatusPreconditionFailed,
		},

		GetTasksHandlerTestCase{
			name: "service acknowledges different policy", reqOrg: org, user: providerUser,
			queryParams: map[string]string{"siteId": site.ID.String(), "eventPolicyId": policyID},
			mockTasks:   listed, expectedStatus: http.StatusPreconditionFailed,
			appliedFilters: &flowv1.TaskListAppliedFilters{EventFilter: &flowv1.EventTaskFilter{PolicyId: &flowv1.UUID{Id: uuid.NewString()}}},
		},
	)
	for name, selectors := range map[string]map[string]string{
		"empty event": {"eventType": ""}, "unsupported event": {"eventType": "unknown"},
		"invalid event policy": {"eventPolicyId": "invalid"}, "zero event policy": {"eventPolicyId": uuid.Nil.String()},
		"Rack belongs in path": {"rackId": "rack-17"}, "Tray belongs in path": {"trayId": "tray-17"}, "ambiguous policy name": {"policyId": policyID},
		"invalid event-only flag":     {"eventTriggeredOnly": "maybe"},
		"source filter not supported": {"sourceType": "Rack", "sourceId": "rack-17"},
	} {
		selectors["siteId"] = site.ID.String()
		cases = append(cases, GetTasksHandlerTestCase{name: name, reqOrg: org, user: providerUser, queryParams: selectors, expectedStatus: http.StatusBadRequest})
	}

	ExecuteGetTasksHandlerTestCases(t, "/v2/org/%s/nico/task", handler.Handle, scp, site.ID.String(), cases)
}

func TestGetRackTasksHandler_Handle(t *testing.T) {
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-task-list-rack", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-task-list-rack", org, []string{authz.TenantAdminRole})

	handler := NewGetRackTasksHandler(dbSession, nil, scp, cfg)
	rackID := "core-rack-01"
	taskUUID := uuid.New().String()
	listed := []*flowv1.Task{{
		Id:          &flowv1.UUID{Id: taskUUID},
		RackId:      &flowv1.UUID{Id: rackID},
		Description: "Power on rack",
		Report:      `{"version":1,"stages":[]}`,
		Status:      flowv1.TaskStatus_TASK_STATUS_RUNNING,
	}}

	cases := []GetTasksHandlerTestCase{
		{
			name:           "success - list rack tasks",
			reqOrg:         org,
			user:           providerUser,
			pathParam:      rackID,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			mockTasks:      listed,
			expectedStatus: http.StatusOK,
			expectedPage:   &pagination.PageResponse{PageNumber: 1, PageSize: 20, Total: 1},
			assertFlowReq: func(t *testing.T, req *flowv1.ListTasksRequest, pathParam string) {
				t.Helper()
				assert.Equal(t, pathParam, req.GetRackId().GetId())
				assert.Nil(t, req.GetComponentId())
				assert.False(t, req.GetActiveOnly())
			},
		},
		{
			name:           "success - active-only filter pass-through",
			reqOrg:         org,
			user:           providerUser,
			pathParam:      rackID,
			queryParams:    map[string]string{"siteId": site.ID.String(), "activeOnly": "true", "pageNumber": "2", "pageSize": "10"},
			mockTasks:      listed,
			expectedStatus: http.StatusOK,
			expectedPage:   &pagination.PageResponse{PageNumber: 2, PageSize: 10, Total: 1},
			assertFlowReq: func(t *testing.T, req *flowv1.ListTasksRequest, pathParam string) {
				t.Helper()
				assert.Equal(t, pathParam, req.GetRackId().GetId())
				assert.True(t, req.GetActiveOnly())
				require.NotNil(t, req.GetPagination())
				assert.Equal(t, int32(10), req.GetPagination().GetOffset())
				assert.Equal(t, int32(10), req.GetPagination().GetLimit())
			},
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			pathParam:      rackID,
			queryParams:    map[string]string{},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			reqOrg:         org,
			user:           tenantUser,
			pathParam:      rackID,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			expectedStatus: http.StatusForbidden,
		},
	}

	cases = append(cases, eventFilteredTaskListCases(GetTasksHandlerTestCase{reqOrg: org, user: providerUser, pathParam: rackID, mockTasks: listed}, site.ID.String(), &flowv1.TaskListAppliedFilters{RackId: &flowv1.UUID{Id: rackID}})...)
	foreignSite := testTaskListForeignSite(t, dbSession, site)
	cases = append(cases,
		GetTasksHandlerTestCase{name: "missing user", reqOrg: org, pathParam: rackID, queryParams: map[string]string{"siteId": site.ID.String()}, expectedStatus: http.StatusInternalServerError},
		GetTasksHandlerTestCase{name: "org membership", reqOrg: "unrelated-org", user: providerUser, pathParam: rackID, queryParams: map[string]string{"siteId": site.ID.String()}, expectedStatus: http.StatusForbidden},
		GetTasksHandlerTestCase{name: "Provider Admin cannot access foreign Site", reqOrg: org, user: providerUser, pathParam: rackID, queryParams: map[string]string{"siteId": foreignSite.ID.String()}, expectedStatus: http.StatusForbidden},
		GetTasksHandlerTestCase{name: "invalid Site UUID", reqOrg: org, user: providerUser, pathParam: rackID, queryParams: map[string]string{"siteId": "invalid"}, expectedStatus: http.StatusBadRequest},
	)

	ExecuteGetTasksHandlerTestCases(t, "/v2/org/%s/nico/rack/%s/task", handler.Handle, scp, site.ID.String(), cases)
}

func TestGetTrayTasksHandler_Handle(t *testing.T) {
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-task-list-tray", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-task-list-tray", org, []string{authz.TenantAdminRole})

	handler := NewGetTrayTasksHandler(dbSession, nil, scp, cfg)
	trayID := "core-machine-01"
	taskUUID := uuid.New().String()
	listed := []*flowv1.Task{{
		Id:          &flowv1.UUID{Id: taskUUID},
		RackId:      &flowv1.UUID{Id: uuid.New().String()},
		Description: "Update tray firmware",
		Report:      `{"version":1,"stages":[]}`,
		Status:      flowv1.TaskStatus_TASK_STATUS_PENDING,
	}}

	cases := []GetTasksHandlerTestCase{
		{
			name:           "success - list tray tasks",
			reqOrg:         org,
			user:           providerUser,
			pathParam:      trayID,
			queryParams:    map[string]string{"siteId": site.ID.String(), "pageSize": "5"},
			mockTasks:      listed,
			expectedStatus: http.StatusOK,
			expectedPage:   &pagination.PageResponse{PageNumber: 1, PageSize: 5, Total: 1},
			assertFlowReq: func(t *testing.T, req *flowv1.ListTasksRequest, pathParam string) {
				t.Helper()
				assert.Equal(t, pathParam, req.GetComponentId().GetId())
				assert.Nil(t, req.GetRackId())
				require.NotNil(t, req.GetPagination())
				assert.Equal(t, int32(0), req.GetPagination().GetOffset())
				assert.Equal(t, int32(5), req.GetPagination().GetLimit())
			},
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			pathParam:      trayID,
			queryParams:    map[string]string{},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			reqOrg:         org,
			user:           tenantUser,
			pathParam:      trayID,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			expectedStatus: http.StatusForbidden,
		},
	}

	cases = append(cases, eventFilteredTaskListCases(GetTasksHandlerTestCase{reqOrg: org, user: providerUser, pathParam: trayID, mockTasks: listed}, site.ID.String(), &flowv1.TaskListAppliedFilters{ComponentId: &flowv1.UUID{Id: trayID}})...)
	foreignSite := testTaskListForeignSite(t, dbSession, site)
	cases = append(cases,
		GetTasksHandlerTestCase{name: "missing user", reqOrg: org, pathParam: trayID, queryParams: map[string]string{"siteId": site.ID.String()}, expectedStatus: http.StatusInternalServerError},
		GetTasksHandlerTestCase{name: "org membership", reqOrg: "unrelated-org", user: providerUser, pathParam: trayID, queryParams: map[string]string{"siteId": site.ID.String()}, expectedStatus: http.StatusForbidden},
		GetTasksHandlerTestCase{name: "Provider Admin cannot access foreign Site", reqOrg: org, user: providerUser, pathParam: trayID, queryParams: map[string]string{"siteId": foreignSite.ID.String()}, expectedStatus: http.StatusForbidden},
		GetTasksHandlerTestCase{name: "invalid Site UUID", reqOrg: org, user: providerUser, pathParam: trayID, queryParams: map[string]string{"siteId": "invalid"}, expectedStatus: http.StatusBadRequest},
	)

	ExecuteGetTasksHandlerTestCases(t, "/v2/org/%s/nico/tray/%s/task", handler.Handle, scp, site.ID.String(), cases)
}

func TestCancelTaskHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	siteNoRLA := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-site-no-flow-cancel",
		Org:                      org,
		InfrastructureProviderID: site.InfrastructureProviderID,
		Status:                   cdbm.SiteStatusRegistered,
		Config:                   &cdbm.SiteConfig{},
	}
	_, err := dbSession.DB.NewInsert().Model(siteNoRLA).Exec(context.Background())
	assert.Nil(t, err)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-task-cancel", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-task-cancel", org, []string{authz.TenantAdminRole})

	handler := NewCancelTaskHandler(dbSession, nil, scp, cfg)

	taskUUID := uuid.New().String()

	cancelledTask := &flowv1.Task{
		Id:          &flowv1.UUID{Id: taskUUID},
		Operation:   "power_on",
		RackId:      &flowv1.UUID{Id: uuid.New().String()},
		Description: "Power on rack",
		Status:      flowv1.TaskStatus_TASK_STATUS_TERMINATED,
		Message:     "Cancelled by user",
	}

	failureConverter := tp.GetDefaultFailureConverter()
	missingTaskError := failureConverter.FailureToError(failureConverter.ErrorToFailure(
		swe.WrapErr(status.Error(codes.NotFound, "task not found")),
	))

	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		taskUUID       string
		body           any
		mockTask       *flowv1.Task
		mockExecErr    error
		mockResultErr  error
		expectedStatus int
		expectNullData bool
	}{
		{
			name:           "success - cancel task returns 202 Accepted",
			reqOrg:         org,
			user:           providerUser,
			taskUUID:       taskUUID,
			body:           model.APICancelTaskRequest{SiteID: site.ID.String()},
			mockTask:       cancelledTask,
			expectedStatus: http.StatusAccepted,
		},
		{
			name:           "failure - Flow not enabled on site",
			reqOrg:         org,
			user:           providerUser,
			taskUUID:       taskUUID,
			body:           model.APICancelTaskRequest{SiteID: siteNoRLA.ID.String()},
			expectedStatus: http.StatusPreconditionFailed,
		},
		{
			name:           "failure - missing siteId",
			reqOrg:         org,
			user:           providerUser,
			taskUUID:       taskUUID,
			body:           model.APICancelTaskRequest{},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - invalid siteId",
			reqOrg:         org,
			user:           providerUser,
			taskUUID:       taskUUID,
			body:           model.APICancelTaskRequest{SiteID: uuid.New().String()},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - invalid task UUID",
			reqOrg:         org,
			user:           providerUser,
			taskUUID:       "not-a-uuid",
			body:           model.APICancelTaskRequest{SiteID: site.ID.String()},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			reqOrg:         org,
			user:           tenantUser,
			taskUUID:       taskUUID,
			body:           model.APICancelTaskRequest{SiteID: site.ID.String()},
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "failure - workflow scheduling error",
			reqOrg:         org,
			user:           providerUser,
			taskUUID:       taskUUID,
			body:           model.APICancelTaskRequest{SiteID: site.ID.String()},
			mockExecErr:    errors.New("temporal scheduling failed"),
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:     "failure - Flow rejects zero task UUID",
			reqOrg:   org,
			user:     providerUser,
			taskUUID: uuid.Nil.String(),
			body:     model.APICancelTaskRequest{SiteID: site.ID.String()},
			mockResultErr: tp.NewNonRetryableApplicationError(
				"task_id must be a valid non-zero UUID",
				swe.ErrTypeNICoInvalidArgument,
				nil,
			),
			expectedStatus: http.StatusBadRequest,
			expectNullData: true,
		},
		{
			name:           "failure - missing task cannot be cancelled",
			reqOrg:         org,
			user:           providerUser,
			taskUUID:       taskUUID,
			body:           model.APICancelTaskRequest{SiteID: site.ID.String()},
			mockResultErr:  missingTaskError,
			expectedStatus: http.StatusNotFound,
			expectNullData: true,
		},
		{
			name:     "failure - completed task cannot be cancelled",
			reqOrg:   org,
			user:     providerUser,
			taskUUID: taskUUID,
			body:     model.APICancelTaskRequest{SiteID: site.ID.String()},
			mockResultErr: tp.NewNonRetryableApplicationError(
				"task cannot be cancelled",
				swe.ErrTypeNICoFailedPrecondition,
				errors.New("task cannot be cancelled (status: failed)"),
			),
			expectedStatus: http.StatusPreconditionFailed,
			expectNullData: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			if tt.mockTask != nil {
				testFlowProxyReply(t, mockWorkflowRun, &flowv1.CancelTaskResponse{Task: tt.mockTask})
			} else if tt.mockResultErr != nil {
				mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Return(tt.mockResultErr)
			}
			testFlowProxyDispatch(t, mockTemporalClient, mockWorkflowRun, flowv1.Flow_CancelTask_FullMethodName, tt.mockExecErr)
			scp.IDClientMap[site.ID.String()] = mockTemporalClient

			path := fmt.Sprintf("/v2/org/%s/nico/rack/task/%s/cancel", tt.reqOrg, tt.taskUUID)

			bodyBytes, err := json.Marshal(tt.body)
			require.NoError(t, err)

			req := httptest.NewRequest(http.MethodPost, path, bytes.NewReader(bodyBytes))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrg, tt.taskUUID)
			ec.Set("user", tt.user)

			ec.SetRequest(ec.Request().WithContext(ctx))

			err = handler.Handle(ec)

			if tt.expectedStatus != rec.Code {
				t.Errorf("CancelTaskHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			require.Equal(t, tt.expectedStatus, ec.Response().Status)
			if tt.expectedStatus != http.StatusAccepted {
				var apiErr map[string]any
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiErr))
				require.NotEmpty(t, apiErr["message"])
				if tt.expectedStatus == http.StatusNotFound {
					var appErr *tp.ApplicationError
					require.ErrorAs(t, tt.mockResultErr, &appErr)
					require.True(t, appErr.NonRetryable())
					require.Equal(t, "Task not found", apiErr["message"])
					require.NotContains(t, rec.Body.String(), "retryable: true")
				}
				if tt.expectNullData {
					require.Contains(t, apiErr, "data")
					assert.Nil(t, apiErr["data"])
				}
				return
			}

			var apiTask model.APITask
			err = json.Unmarshal(rec.Body.Bytes(), &apiTask)
			assert.NoError(t, err)
			assert.Equal(t, taskUUID, apiTask.ID)
			assert.Equal(t, "Terminated", apiTask.Status)
			assert.Equal(t, "Cancelled by user", apiTask.Message)
		})
	}
}

// eventFilteredTaskListCases proves that scoped handlers compose the path target
// with the common query, preserve paging/report options, and reject dropped filters.
func eventFilteredTaskListCases(base GetTasksHandlerTestCase, siteID string, target *flowv1.TaskListAppliedFilters) []GetTasksHandlerTestCase {
	const policyID = "63f1c140-81c9-4711-8c3a-1664aaed2cd9"
	filter := &flowv1.EventTaskFilter{EventType: proto.String("hardware.leak.detected"), PolicyId: &flowv1.UUID{Id: policyID}}
	applied := proto.Clone(target).(*flowv1.TaskListAppliedFilters)
	applied.EventFilter = filter
	wrongTarget := proto.Clone(applied).(*flowv1.TaskListAppliedFilters)
	if wrongTarget.RackId != nil {
		wrongTarget.RackId.Id = "another-rack"
	} else {
		wrongTarget.ComponentId.Id = "another-tray"
	}
	var result []GetTasksHandlerTestCase
	for _, tc := range []struct {
		name     string
		query    map[string]string
		applied  *flowv1.TaskListAppliedFilters
		code     int
		proxyErr error
		invalid  bool
	}{
		{name: "event provenance target report and page selectors", query: map[string]string{"eventType": "hardware.leak.detected", "eventPolicyId": policyID, "eventTriggeredOnly": "false", "activeOnly": "true", "includeReport": "true", "pageNumber": "2", "pageSize": "1"}, applied: applied, code: http.StatusOK},
		{name: "older service drops event filters", query: map[string]string{"eventTriggeredOnly": "true"}, code: http.StatusPreconditionFailed},
		{name: "service acknowledges another target", query: map[string]string{"eventType": "hardware.leak.detected", "eventPolicyId": policyID}, applied: wrongTarget, code: http.StatusPreconditionFailed},
		{name: "service drops target acknowledgement", query: map[string]string{"eventType": "hardware.leak.detected", "eventPolicyId": policyID}, applied: &flowv1.TaskListAppliedFilters{EventFilter: filter}, code: http.StatusPreconditionFailed},
		{name: "unsupported event rejected before dispatch", query: map[string]string{"eventType": "future.event"}, code: http.StatusBadRequest, invalid: true},
		{name: "target selector belongs in path", query: map[string]string{"rackId": "other", "trayId": "other"}, code: http.StatusBadRequest, invalid: true},
		{name: "missing path target", query: map[string]string{"eventTriggeredOnly": "true"}, code: http.StatusNotFound, proxyErr: tp.NewNonRetryableApplicationError("target not found", swe.ErrTypeNICoObjectNotFound, nil)},
	} {
		item := base
		item.name = tc.name
		item.queryParams = tc.query
		item.queryParams["siteId"] = siteID
		item.appliedFilters = tc.applied
		item.expectedStatus = tc.code
		item.proxyErr = tc.proxyErr
		if tc.invalid || tc.proxyErr != nil {
			item.mockTasks = nil
		}
		if tc.code == http.StatusOK {
			item.mockTotal = proto.Int32(3)
			item.expectedPage = &pagination.PageResponse{PageNumber: 2, PageSize: 1, Total: 3}
			item.assertFlowReq = func(t *testing.T, req *flowv1.ListTasksRequest, pathParam string) {
				require.True(t, proto.Equal(filter, req.EventFilter))
				if target.RackId != nil {
					require.Equal(t, pathParam, req.GetRackId().GetId())
					require.Nil(t, req.ComponentId)
				} else {
					require.Equal(t, pathParam, req.GetComponentId().GetId())
					require.Nil(t, req.RackId)
				}
				require.True(t, req.ActiveOnly)
				require.True(t, req.WithReport)
				require.EqualValues(t, 1, req.GetPagination().GetOffset())
				require.EqualValues(t, 1, req.GetPagination().GetLimit())
			}
			item.assertResponse = func(t *testing.T, tasks []model.APITask) { require.NotNil(t, tasks[0].Report) }
		}
		result = append(result, item)
	}
	return result
}

func testTaskListForeignSite(t *testing.T, db *cdb.Session, site *cdbm.Site) *cdbm.Site {
	t.Helper()
	provider := &cdbm.InfrastructureProvider{ID: uuid.New(), Name: "foreign Task Provider", Org: "foreign-task-org"}
	_, err := db.DB.NewInsert().Model(provider).Exec(t.Context())
	require.NoError(t, err)
	foreign := *site
	foreign.ID = uuid.New()
	foreign.Name = "foreign-task-site"
	foreign.Org = provider.Org
	foreign.InfrastructureProviderID = provider.ID
	_, err = db.DB.NewInsert().Model(&foreign).Exec(t.Context())
	require.NoError(t, err)
	return &foreign
}
