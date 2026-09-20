// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/mocks"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestRackHealthReportHandler_Handle(t *testing.T) {
	fixture := common.NewTestSetupProviderMachineHandlerFixture(t, &corev1.ListHealthReportResponse{
		HealthReportEntries: []*corev1.HealthReportEntry{{
			Mode: corev1.HealthReportApplyMode_Merge,
			Report: &corev1.HealthReport{
				Source: "overrides.sre",
				Alerts: []*corev1.HealthProbeAlert{{Id: "probe.alert", Message: "forced unhealthy"}},
			},
		}},
	})
	unauthorizedUser := common.TestBuildUser(t, fixture.DBSession, "unauthorized-starfleet-id", fixture.Org, []string{auth.TenantAdminRole})

	tests := []struct {
		name           string
		factory        func() echo.HandlerFunc
		method         string
		query          string
		body           any
		source         string
		wantStatus     int
		wantCoreMethod string
		user           any
		coreErr        error
	}{
		{
			name: "list",
			factory: func() echo.HandlerFunc {
				h := NewGetAllRackHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodGet, query: "?siteId=" + fixture.SiteID, wantStatus: http.StatusOK,
			wantCoreMethod: corev1.Forge_ListRackHealthReports_FullMethodName,
		},
		{
			name: "insert",
			factory: func() echo.HandlerFunc {
				h := NewCreateOrUpdateRackHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodPut, body: validRackHealthReportRequest(fixture.SiteID), wantStatus: http.StatusOK,
			wantCoreMethod: corev1.Forge_InsertRackHealthReport_FullMethodName,
		},
		{
			name: "remove",
			factory: func() echo.HandlerFunc {
				h := NewDeleteRackHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodDelete, query: "?siteId=" + fixture.SiteID, source: "overrides.sre", wantStatus: http.StatusNoContent,
			wantCoreMethod: corev1.Forge_RemoveRackHealthReport_FullMethodName,
		},
		{
			name: "reject missing site ID",
			factory: func() echo.HandlerFunc {
				h := NewGetAllRackHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodGet, wantStatus: http.StatusBadRequest,
		},
		{
			name: "reject unknown query parameter",
			factory: func() echo.HandlerFunc {
				h := NewGetAllRackHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodGet, query: "?siteId=" + fixture.SiteID + "&unexpected=value", wantStatus: http.StatusBadRequest,
		},
		{
			name: "reject unauthorized request",
			factory: func() echo.HandlerFunc {
				h := NewGetAllRackHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodGet, query: "?siteId=" + fixture.SiteID, wantStatus: http.StatusForbidden,
			user: unauthorizedUser,
		},
		{
			name: "preserve Core not found status",
			factory: func() echo.HandlerFunc {
				h := NewGetAllRackHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodGet, query: "?siteId=" + fixture.SiteID, wantStatus: http.StatusNotFound,
			wantCoreMethod: corev1.Forge_ListRackHealthReports_FullMethodName,
			coreErr:        status.Error(codes.NotFound, "rack not found"),
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			*fixture.ProxiedReq = grpcproxy.Request{}
			originalClient := fixture.SiteClientPool.IDClientMap[fixture.SiteID]
			if test.coreErr != nil {
				wrun := &mocks.WorkflowRun{}
				wrun.On("Get", mock.Anything, mock.Anything).Return(test.coreErr)
				tsc := &mocks.Client{}
				tsc.On(
					"ExecuteWorkflow",
					mock.Anything,
					mock.Anything,
					grpcproxy.Core.WorkflowName,
					mock.MatchedBy(func(req grpcproxy.Request) bool {
						*fixture.ProxiedReq = req
						return true
					}),
				).Return(wrun, nil)
				fixture.SiteClientPool.IDClientMap[fixture.SiteID] = tsc
				t.Cleanup(func() {
					fixture.SiteClientPool.IDClientMap[fixture.SiteID] = originalClient
				})
			}

			requestFixture := fixture
			if test.user != nil {
				requestFixture.User = test.user
			}
			rec := requestFixture.Request(t, test.factory(), test.method, "/"+test.query, test.body, test.source)

			assert.Equal(t, test.wantStatus, rec.Code, rec.Body.String())
			assert.Equal(t, test.wantCoreMethod, fixture.ProxiedReq.FullMethod)
			if test.wantCoreMethod == "" {
				return
			}
			assert.Empty(t, fixture.ProxiedReq.EncryptedSecrets)
			wantSource := test.source
			if test.method == http.MethodPut {
				wantSource = "overrides.sre"
			}
			assertRackHealthCoreRequest(t, fixture.ProxiedReq, fixture.MachineID, wantSource)
			assert.NotContains(t, rec.Body.String(), "password")
			if test.method == http.MethodGet && test.wantStatus == http.StatusOK {
				assert.Contains(t, rec.Body.String(), "overrides.sre")
			}
		})
	}
}

func validRackHealthReportRequest(siteID string) model.APIRackHealthReportEntryRequest {
	return model.APIRackHealthReportEntryRequest{
		SiteID: siteID,
		APIMachineHealthReportEntryRequest: model.APIMachineHealthReportEntryRequest{
			Source:    "overrides.sre",
			Mode:      model.MachineHealthReportModeMerge,
			Successes: []model.APIMachineHealthProbeSuccess{{ID: "probe.ok"}},
		},
	}
}

func assertRackHealthCoreRequest(t *testing.T, req *grpcproxy.Request, wantID, wantSource string) {
	t.Helper()

	var gotID, gotSource string
	switch req.FullMethod {
	case corev1.Forge_ListRackHealthReports_FullMethodName:
		var value corev1.ListRackHealthReportsRequest
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID = value.GetRackId().GetId()
	case corev1.Forge_InsertRackHealthReport_FullMethodName:
		var value corev1.InsertRackHealthReportRequest
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID, gotSource = value.GetRackId().GetId(), value.GetHealthReportEntry().GetReport().GetSource()
	case corev1.Forge_RemoveRackHealthReport_FullMethodName:
		var value corev1.RemoveRackHealthReportRequest
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID, gotSource = value.GetRackId().GetId(), value.GetSource()
	default:
		require.FailNow(t, "unexpected Core method", req.FullMethod)
	}

	assert.Equal(t, wantID, gotID)
	assert.Equal(t, wantSource, gotSource)
}
