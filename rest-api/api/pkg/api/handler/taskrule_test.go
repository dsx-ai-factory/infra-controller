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
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	oteltrace "go.opentelemetry.io/otel/trace"
	temporalEnums "go.temporal.io/api/enums/v1"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// testRuleSampleAPIRequest returns a minimal valid create-rule API body.
func testRuleSampleAPIRequest(siteID string) model.APITaskRuleCreateRequest {
	return model.APITaskRuleCreateRequest{
		SiteID:        siteID,
		Name:          "rule-1",
		Description:   "test rule",
		OperationType: model.APIOperationTypePowerControl,
		OperationCode: "power_on",
		RuleDefinition: model.APITaskRuleDefinition{
			Version: "v1",
			Steps: []model.APITaskRuleSequenceStep{
				{
					ComponentType: "Compute",
					Stage:         1,
					MaxParallel:   4,
					MainOperation: model.APITaskRuleActionConfig{Name: "PowerControl"},
				},
			},
		},
	}
}

func TestCreateRuleHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	siteNoFlow := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-site-no-flow-rule-create",
		Org:                      org,
		InfrastructureProviderID: site.InfrastructureProviderID,
		Status:                   cdbm.SiteStatusRegistered,
		Config:                   &cdbm.SiteConfig{},
	}
	_, err := dbSession.DB.NewInsert().Model(siteNoFlow).Exec(context.Background())
	require.NoError(t, err)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-rule-create", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-rule-create", org, []string{authz.TenantAdminRole})

	handler := NewCreateTaskRuleHandler(dbSession, nil, scp, cfg)

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")

	tests := []struct {
		name           string
		reqOrg         string
		user           *cdbm.User
		body           any
		mockResp       *flowv1.CreateOperationRuleResponse
		mockExecErr    error
		expectedStatus int
	}{
		{
			name:           "success - 201 with rule body",
			reqOrg:         org,
			user:           providerUser,
			body:           testRuleSampleAPIRequest(site.ID.String()),
			mockResp:       &flowv1.CreateOperationRuleResponse{Id: &flowv1.UUID{Id: uuid.New().String()}},
			expectedStatus: http.StatusCreated,
		},
		{
			name:           "failure - Flow not enabled on site",
			reqOrg:         org,
			user:           providerUser,
			body:           testRuleSampleAPIRequest(siteNoFlow.ID.String()),
			expectedStatus: http.StatusPreconditionFailed,
		},
		{
			name:   "failure - missing siteId",
			reqOrg: org,
			user:   providerUser,
			body: func() model.APITaskRuleCreateRequest {
				r := testRuleSampleAPIRequest(site.ID.String())
				r.SiteID = ""
				return r
			}(),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:   "failure - invalid operationType",
			reqOrg: org,
			user:   providerUser,
			body: func() model.APITaskRuleCreateRequest {
				r := testRuleSampleAPIRequest(site.ID.String())
				r.OperationType = "bogus"
				return r
			}(),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			reqOrg:         org,
			user:           tenantUser,
			body:           testRuleSampleAPIRequest(site.ID.String()),
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "failure - workflow scheduling error",
			reqOrg:         org,
			user:           providerUser,
			body:           testRuleSampleAPIRequest(site.ID.String()),
			mockExecErr:    errors.New("temporal scheduling failed"),
			expectedStatus: http.StatusInternalServerError,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTC := &tmocks.Client{}
			mockRun := &tmocks.WorkflowRun{}
			mockRun.On("GetID").Return("test-workflow-id")
			if tt.mockResp != nil {
				testFlowProxyReply(t, mockRun, &flowv1.CreateOperationRuleResponse{Id: tt.mockResp.Id})
			}
			started := testFlowProxyDispatch(t, mockTC, mockRun, flowv1.Flow_CreateOperationRule_FullMethodName, tt.mockExecErr)
			scp.IDClientMap[site.ID.String()] = mockTC

			bodyBytes, err := json.Marshal(tt.body)
			require.NoError(t, err)

			req := httptest.NewRequest(http.MethodPost, fmt.Sprintf("/v2/org/%s/nico/task/rule", tt.reqOrg), bytes.NewReader(bodyBytes))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.reqOrg)
			ec.Set("user", tt.user)

			ctx := context.WithValue(context.Background(), otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			_ = handler.Handle(ec)
			require.Equal(t, tt.expectedStatus, rec.Code, "body=%s", rec.Body.String())

			if tt.expectedStatus != http.StatusCreated {
				return
			}

			// A per-request ID is what keeps two creates from becoming one
			// rule, so the policy that resolves a collision never applies.
			assert.True(t, strings.HasPrefix(started.ID, "task-rule-create-"), "workflow ID = %q", started.ID)
			assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED, started.WorkflowIDConflictPolicy)

			var got model.APITaskRule
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &got))
			assert.Equal(t, tt.mockResp.GetId().GetId(), got.ID)
			assert.Equal(t, "rule-1", got.Name)
			assert.Equal(t, model.APIOperationTypePowerControl, got.OperationType)
		})
	}
}

func TestGetRuleHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)

	providerUser := testRackBuildUser(t, dbSession, "provider-user-rule-get", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-rule-get", org, []string{authz.TenantAdminRole})

	handler := NewGetTaskRuleHandler(dbSession, nil, scp, cfg)

	ruleID := uuid.New().String()
	rdJSON, err := json.Marshal(testRuleSampleAPIRequest(site.ID.String()).RuleDefinition)
	require.NoError(t, err)

	mockRule := &flowv1.OperationRule{
		Id:                 &flowv1.UUID{Id: ruleID},
		Name:               "rule-1",
		Description:        "test",
		OperationType:      flowv1.OperationType_OPERATION_TYPE_POWER_CONTROL,
		OperationCode:      "power_on",
		RuleDefinitionJson: string(rdJSON),
		CreatedAt:          timestamppb.Now(),
		UpdatedAt:          timestamppb.Now(),
	}

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")

	tests := []struct {
		name           string
		user           *cdbm.User
		ruleID         string
		queryParams    map[string]string
		mockRule       *flowv1.OperationRule
		expectedStatus int
	}{
		{
			name:           "success - 200 with rule",
			user:           providerUser,
			ruleID:         ruleID,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			mockRule:       mockRule,
			expectedStatus: http.StatusOK,
		},
		{
			name:           "failure - rule not found",
			user:           providerUser,
			ruleID:         ruleID,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			mockRule:       &flowv1.OperationRule{},
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "failure - missing siteId",
			user:           providerUser,
			ruleID:         ruleID,
			queryParams:    map[string]string{},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - invalid rule UUID",
			user:           providerUser,
			ruleID:         "not-a-uuid",
			queryParams:    map[string]string{"siteId": site.ID.String()},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			user:           tenantUser,
			ruleID:         ruleID,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			expectedStatus: http.StatusForbidden,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTC := &tmocks.Client{}
			mockRun := &tmocks.WorkflowRun{}
			mockRun.On("GetID").Return("test-workflow-id")
			if tt.mockRule != nil {
				testFlowProxyReply(t, mockRun, tt.mockRule)
			}
			testFlowProxyDispatch(t, mockTC, mockRun, flowv1.Flow_GetOperationRule_FullMethodName, nil)
			scp.IDClientMap[site.ID.String()] = mockTC

			q := url.Values{}
			for k, v := range tt.queryParams {
				q.Set(k, v)
			}
			path := fmt.Sprintf("/v2/org/%s/nico/task/rule/%s?%s", org, tt.ruleID, q.Encode())
			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(org, tt.ruleID)
			ec.Set("user", tt.user)
			ctx := context.WithValue(context.Background(), otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			_ = handler.Handle(ec)
			require.Equal(t, tt.expectedStatus, rec.Code, "body=%s", rec.Body.String())

			if tt.expectedStatus != http.StatusOK {
				return
			}
			var got model.APITaskRule
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &got))
			assert.Equal(t, ruleID, got.ID)
			assert.Equal(t, "rule-1", got.Name)
			assert.Equal(t, model.APIOperationTypePowerControl, got.OperationType)
		})
	}
}

func TestListRulesHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)
	providerUser := testRackBuildUser(t, dbSession, "provider-user-rule-list", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-rule-list", org, []string{authz.TenantAdminRole})

	handler := NewGetAllTaskRuleHandler(dbSession, nil, scp, cfg)

	rdJSON, err := json.Marshal(testRuleSampleAPIRequest(site.ID.String()).RuleDefinition)
	require.NoError(t, err)

	listed := []*flowv1.OperationRule{
		{
			Id:                 &flowv1.UUID{Id: uuid.New().String()},
			Name:               "rule-1",
			OperationType:      flowv1.OperationType_OPERATION_TYPE_POWER_CONTROL,
			OperationCode:      "power_on",
			RuleDefinitionJson: string(rdJSON),
			CreatedAt:          timestamppb.Now(),
			UpdatedAt:          timestamppb.Now(),
		},
	}

	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")

	tests := []struct {
		name           string
		user           *cdbm.User
		queryParams    map[string]string
		mockRules      []*flowv1.OperationRule
		expectedStatus int
		assertFlowReq  func(t *testing.T, req *flowv1.ListOperationRulesRequest)
	}{
		{
			name:           "success - list",
			user:           providerUser,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			mockRules:      listed,
			expectedStatus: http.StatusOK,
		},
		{
			name: "success - filters pass through",
			user: providerUser,
			queryParams: map[string]string{
				"siteId":        site.ID.String(),
				"operationType": string(model.APIOperationTypePowerControl),
			},
			mockRules:      listed,
			expectedStatus: http.StatusOK,
			assertFlowReq: func(t *testing.T, req *flowv1.ListOperationRulesRequest) {
				t.Helper()
				require.NotNil(t, req.OperationType)
				assert.Equal(t, flowv1.OperationType_OPERATION_TYPE_POWER_CONTROL, *req.OperationType)
			},
		},
		{
			name:           "failure - missing siteId",
			user:           providerUser,
			queryParams:    map[string]string{},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - invalid operationType",
			user:           providerUser,
			queryParams:    map[string]string{"siteId": site.ID.String(), "operationType": "bogus"},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			user:           tenantUser,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			expectedStatus: http.StatusForbidden,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTC := &tmocks.Client{}
			mockRun := &tmocks.WorkflowRun{}
			mockRun.On("GetID").Return("test-workflow-id")
			if tt.mockRules != nil {
				testFlowProxyReply(t, mockRun, &flowv1.ListOperationRulesResponse{
					Rules:      tt.mockRules,
					TotalCount: int32(len(tt.mockRules)),
				})
			}
			mockTC.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Flow.WorkflowName, mock.Anything).
				Run(func(args mock.Arguments) {
					assert.Equal(t, flowv1.Flow_ListOperationRules_FullMethodName, args.Get(3).(grpcproxy.Request).FullMethod)
					if tt.assertFlowReq != nil {
						req := &flowv1.ListOperationRulesRequest{}
						testFlowProxyRequest(t, args, req)
						tt.assertFlowReq(t, req)
					}
				}).
				Return(mockRun, nil)
			scp.IDClientMap[site.ID.String()] = mockTC

			q := url.Values{}
			for k, v := range tt.queryParams {
				q.Set(k, v)
			}
			path := fmt.Sprintf("/v2/org/%s/nico/task/rule?%s", org, q.Encode())
			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(org)
			ec.Set("user", tt.user)
			ctx := context.WithValue(context.Background(), otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			_ = handler.Handle(ec)
			require.Equal(t, tt.expectedStatus, rec.Code, "body=%s", rec.Body.String())

			if tt.expectedStatus != http.StatusOK {
				return
			}
			var got []*model.APITaskRule
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &got))
			require.Len(t, got, len(tt.mockRules))
			require.NotEmpty(t, rec.Header().Get("X-Pagination"))
		})
	}
}

func TestUpdateRuleHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)
	providerUser := testRackBuildUser(t, dbSession, "provider-user-rule-update", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-rule-update", org, []string{authz.TenantAdminRole})

	handler := NewUpdateTaskRuleHandler(dbSession, nil, scp, cfg)

	ruleID := uuid.New().String()
	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")

	name := "renamed"
	tests := []struct {
		name           string
		user           *cdbm.User
		ruleID         string
		body           any
		mockExecErr    error
		mockGetErr     error
		expectedStatus int
	}{
		{
			name:           "success - 204",
			user:           providerUser,
			ruleID:         ruleID,
			body:           model.APITaskRuleUpdateRequest{SiteID: site.ID.String(), Name: &name},
			expectedStatus: http.StatusNoContent,
		},
		{
			name:           "failure - invalid rule UUID",
			user:           providerUser,
			ruleID:         "not-a-uuid",
			body:           model.APITaskRuleUpdateRequest{SiteID: site.ID.String(), Name: &name},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - missing siteId",
			user:           providerUser,
			ruleID:         ruleID,
			body:           model.APITaskRuleUpdateRequest{Name: &name},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - no fields to update",
			user:           providerUser,
			ruleID:         ruleID,
			body:           model.APITaskRuleUpdateRequest{SiteID: site.ID.String()},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			user:           tenantUser,
			ruleID:         ruleID,
			body:           model.APITaskRuleUpdateRequest{SiteID: site.ID.String(), Name: &name},
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "failure - workflow scheduling error",
			user:           providerUser,
			ruleID:         ruleID,
			body:           model.APITaskRuleUpdateRequest{SiteID: site.ID.String(), Name: &name},
			mockExecErr:    errors.New("temporal scheduling failed"),
			expectedStatus: http.StatusInternalServerError,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTC := &tmocks.Client{}
			mockRun := &tmocks.WorkflowRun{}
			mockRun.On("GetID").Return("test-workflow-id")
			mockRun.Mock.On("Get", mock.Anything, mock.Anything).Return(tt.mockGetErr)
			started := testFlowProxyDispatch(t, mockTC, mockRun, flowv1.Flow_UpdateOperationRule_FullMethodName, tt.mockExecErr)
			scp.IDClientMap[site.ID.String()] = mockTC

			bodyBytes, err := json.Marshal(tt.body)
			require.NoError(t, err)
			path := fmt.Sprintf("/v2/org/%s/nico/task/rule/%s", org, tt.ruleID)
			req := httptest.NewRequest(http.MethodPatch, path, bytes.NewReader(bodyBytes))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(org, tt.ruleID)
			ec.Set("user", tt.user)
			ctx := context.WithValue(context.Background(), otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			_ = handler.Handle(ec)
			require.Equal(t, tt.expectedStatus, rec.Code, "body=%s", rec.Body.String())

			if tt.expectedStatus != http.StatusNoContent {
				return
			}

			// Concurrent updates to one rule must stay separate executions, so
			// the ID carries a per-request suffix and never resolves a
			// collision by reading another request's result.
			assert.True(t, strings.HasPrefix(started.ID, fmt.Sprintf("task-rule-update-%s-", tt.ruleID)), "workflow ID = %q", started.ID)
			assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED, started.WorkflowIDConflictPolicy)
		})
	}
}

func TestDeleteRuleHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)
	providerUser := testRackBuildUser(t, dbSession, "provider-user-rule-delete", org, []string{authz.ProviderAdminRole})
	tenantUser := testRackBuildUser(t, dbSession, "tenant-user-rule-delete", org, []string{authz.TenantAdminRole})

	handler := NewDeleteTaskRuleHandler(dbSession, nil, scp, cfg)

	ruleID := uuid.New().String()
	tracer := oteltrace.NewNoopTracerProvider().Tracer("test")

	tests := []struct {
		name           string
		user           *cdbm.User
		ruleID         string
		queryParams    map[string]string
		expectedStatus int
	}{
		{
			name:           "success - 204",
			user:           providerUser,
			ruleID:         ruleID,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			expectedStatus: http.StatusNoContent,
		},
		{
			name:           "failure - invalid rule UUID",
			user:           providerUser,
			ruleID:         "not-a-uuid",
			queryParams:    map[string]string{"siteId": site.ID.String()},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - missing siteId",
			user:           providerUser,
			ruleID:         ruleID,
			queryParams:    map[string]string{},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure - tenant access denied",
			user:           tenantUser,
			ruleID:         ruleID,
			queryParams:    map[string]string{"siteId": site.ID.String()},
			expectedStatus: http.StatusForbidden,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockTC := &tmocks.Client{}
			mockRun := &tmocks.WorkflowRun{}
			mockRun.On("GetID").Return("test-workflow-id")
			mockRun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
			testFlowProxyDispatch(t, mockTC, mockRun, flowv1.Flow_DeleteOperationRule_FullMethodName, nil)
			scp.IDClientMap[site.ID.String()] = mockTC

			q := url.Values{}
			for k, v := range tt.queryParams {
				q.Set(k, v)
			}
			path := fmt.Sprintf("/v2/org/%s/nico/task/rule/%s?%s", org, tt.ruleID, q.Encode())
			req := httptest.NewRequest(http.MethodDelete, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(org, tt.ruleID)
			ec.Set("user", tt.user)
			ctx := context.WithValue(context.Background(), otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			_ = handler.Handle(ec)
			require.Equal(t, tt.expectedStatus, rec.Code, "body=%s", rec.Body.String())
		})
	}
}
