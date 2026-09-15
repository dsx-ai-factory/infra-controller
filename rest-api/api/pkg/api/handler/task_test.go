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
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	oteltrace "go.opentelemetry.io/otel/trace"
	tClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
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
		Status:      flowv1.TaskStatus_TASK_STATUS_RUNNING,
		Message:     "Processing",
	}

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		taskUUID       string
		queryParams    map[string]string
		mockTasks      []*flowv1.Task
		expectedStatus int
	}{
		{
			name:     "success - get task by ID",
			reqOrg:   org,
			user:     providerUser,
			taskUUID: taskUUID,
			queryParams: map[string]string{
				"siteId": site.ID.String(),
			},
			mockTasks:      []*flowv1.Task{mockTask},
			expectedStatus: http.StatusOK,
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

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)

			if tt.expectedStatus != rec.Code {
				t.Errorf("GetTaskHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusOK {
				return
			}

			var apiTask model.APITask
			err = json.Unmarshal(rec.Body.Bytes(), &apiTask)
			assert.NoError(t, err)
			assert.Equal(t, taskUUID, apiTask.ID)
			assert.Equal(t, "Running", apiTask.Status)
			assert.Equal(t, "Power on rack", apiTask.Description)
			assert.Equal(t, "Processing", apiTask.Message)
		})
	}
}

// ExecuteGetTasksHandlerTestCases exercises the root, Rack, and Tray task-list
// handlers with a shared case matrix. pathFmt and the path parameter differ per
// handler; all invoke Flow ListTasks through the generic proxy.
type GetTasksHandlerTestCase struct {
	name           string
	reqOrg         string
	user           *cdbm.User
	pathParam      string
	queryParams    map[string]string
	mockTasks      []*flowv1.Task
	expectedStatus int
	expectedPage   *pagination.PageResponse
	expectedFlowID string
	assertFlowReq  func(t *testing.T, req *flowv1.ListTasksRequest, pathParam string)
	assertResponse func(t *testing.T, tasks []model.APITask)
}

func ExecuteGetTasksHandlerTestCases(t *testing.T, pathFmt string, handle func(echo.Context) error, scp *sc.ClientPool, siteID string, testCases []GetTasksHandlerTestCase) {
	t.Helper()
	e := echo.New()
	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	for _, tt := range testCases {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			if tt.mockTasks != nil {
				testFlowProxyReply(t, mockWorkflowRun, &flowv1.ListTasksResponse{
					Tasks: tt.mockTasks,
					Total: int32(len(tt.mockTasks)),
				})
			}
			mockTemporalClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Flow.WorkflowName, mock.Anything).
				Run(func(args mock.Arguments) {
					assert.Equal(t, flowv1.Flow_ListTasks_FullMethodName, args.Get(3).(grpcproxy.Request).FullMethod)
					if tt.expectedFlowID != "" {
						options := args.Get(1).(tClient.StartWorkflowOptions)
						assert.Equal(t, tt.expectedFlowID, options.ID)
					}
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

			ctx := context.WithValue(context.Background(), otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handle(ec)
			require.Equal(t, tt.expectedStatus, rec.Code, "body=%s err=%v", rec.Body.String(), err)

			if tt.expectedStatus != http.StatusOK {
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
		})
	}
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

	providerUser := testRackBuildUser(t, dbSession, "provider-user-task-list-site", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-task-list-site", org, []string{authz.TenantAdminRole})

	handler := NewGetAllTaskHandler(dbSession, scp)
	taskUUID := uuid.New().String()
	listed := []*flowv1.Task{{
		Id:          &flowv1.UUID{Id: taskUUID},
		RackId:      &flowv1.UUID{Id: uuid.New().String()},
		Description: "Power on rack",
		Status:      flowv1.TaskStatus_TASK_STATUS_RUNNING,
		Report:      `{"version":1,"stages":[]}`,
	}}
	defaultPageNumber, defaultPageSize := 1, 20
	filteredPageNumber, filteredPageSize := 2, 10
	defaultFlowID := fmt.Sprintf("task-get-all-%s", common.QueryParamHash(
		(&model.APIGetTasksRequest{SiteID: site.ID.String()}).QueryValues(pagination.PageRequest{
			PageNumber: &defaultPageNumber,
			PageSize:   &defaultPageSize,
		}),
	))
	filteredFlowID := fmt.Sprintf("task-get-all-%s", common.QueryParamHash(
		(&model.APIGetTasksRequest{SiteID: site.ID.String(), ActiveOnly: true, IncludeReport: true}).QueryValues(pagination.PageRequest{
			PageNumber: &filteredPageNumber,
			PageSize:   &filteredPageSize,
		}),
	))

	cases := []GetTasksHandlerTestCase{
		{
			name:           "success - list every task in site",
			reqOrg:         org,
			user:           providerUser,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			mockTasks:      listed,
			expectedStatus: http.StatusOK,
			expectedPage:   &pagination.PageResponse{PageNumber: 1, PageSize: 20, Total: 1},
			expectedFlowID: defaultFlowID,
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
			name:           "success - filters and pagination pass through",
			reqOrg:         org,
			user:           providerUser,
			queryParams:    map[string]string{"siteId": site.ID.String(), "activeOnly": "true", "includeReport": "true", "pageNumber": "2", "pageSize": "10"},
			mockTasks:      listed,
			expectedStatus: http.StatusOK,
			expectedPage:   &pagination.PageResponse{PageNumber: 2, PageSize: 10, Total: 1},
			expectedFlowID: filteredFlowID,
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

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")
	ctx := context.Background()

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		taskUUID       string
		body           any
		mockTask       *flowv1.Task
		mockExecErr    error
		expectedStatus int
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
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTemporalClient := &tmocks.Client{}
			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return("test-workflow-id")
			if tt.mockTask != nil {
				testFlowProxyReply(t, mockWorkflowRun, &flowv1.CancelTaskResponse{Task: tt.mockTask})
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

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err = handler.Handle(ec)

			if tt.expectedStatus != rec.Code {
				t.Errorf("CancelTaskHandler.Handle() status = %v, want %v, response: %v, err: %v", rec.Code, tt.expectedStatus, rec.Body.String(), err)
			}

			require.Equal(t, tt.expectedStatus, rec.Code)
			if tt.expectedStatus != http.StatusAccepted {
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
