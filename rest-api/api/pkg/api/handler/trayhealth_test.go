// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	testSwitchID     = "sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg"
	testPowerShelfID = "ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg"
)

func TestTrayHealthReportHandler_Handle(t *testing.T) {
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
		name             string
		factory          func() echo.HandlerFunc
		method           string
		query            string
		body             any
		source           string
		resourceID       string
		wantStatus       int
		wantCoreMethod   string
		wantBodyExcludes string
		user             any
	}{
		{
			name: "list Compute",
			factory: func() echo.HandlerFunc {
				h := NewGetAllTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodGet, query: "?siteId=" + fixture.SiteID + "&type=Compute", wantStatus: http.StatusOK,
			wantCoreMethod: corev1.Forge_ListMachineHealthReports_FullMethodName,
		},
		{
			name: "insert Compute",
			factory: func() echo.HandlerFunc {
				h := NewCreateOrUpdateTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodPut, body: validTrayHealthReportRequest(fixture.SiteID, "Compute"), wantStatus: http.StatusOK,
			wantCoreMethod: corev1.Forge_InsertMachineHealthReport_FullMethodName,
		},
		{
			name: "remove Compute",
			factory: func() echo.HandlerFunc {
				h := NewDeleteTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodDelete, query: "?siteId=" + fixture.SiteID + "&type=Compute", source: "overrides.sre", wantStatus: http.StatusNoContent,
			wantCoreMethod: corev1.Forge_RemoveMachineHealthReport_FullMethodName,
		},
		{
			name: "list NVSwitch",
			factory: func() echo.HandlerFunc {
				h := NewGetAllTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodGet, query: "?siteId=" + fixture.SiteID + "&type=NVSwitch", resourceID: testSwitchID, wantStatus: http.StatusOK,
			wantCoreMethod: corev1.Forge_ListSwitchHealthReports_FullMethodName,
		},
		{
			name: "insert NVSwitch",
			factory: func() echo.HandlerFunc {
				h := NewCreateOrUpdateTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodPut, body: validTrayHealthReportRequest(fixture.SiteID, "NVSwitch"), resourceID: testSwitchID, wantStatus: http.StatusOK,
			wantCoreMethod: corev1.Forge_InsertSwitchHealthReport_FullMethodName,
		},
		{
			name: "remove NVSwitch",
			factory: func() echo.HandlerFunc {
				h := NewDeleteTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodDelete, query: "?siteId=" + fixture.SiteID + "&type=NVSwitch", source: "overrides.sre", resourceID: testSwitchID, wantStatus: http.StatusNoContent,
			wantCoreMethod: corev1.Forge_RemoveSwitchHealthReport_FullMethodName,
		},
		{
			name: "list PowerShelf",
			factory: func() echo.HandlerFunc {
				h := NewGetAllTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodGet, query: "?siteId=" + fixture.SiteID + "&type=PowerShelf", resourceID: testPowerShelfID, wantStatus: http.StatusOK,
			wantCoreMethod: corev1.Forge_ListPowerShelfHealthReports_FullMethodName,
		},
		{
			name: "insert PowerShelf",
			factory: func() echo.HandlerFunc {
				h := NewCreateOrUpdateTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodPut, body: validTrayHealthReportRequest(fixture.SiteID, "PowerShelf"), resourceID: testPowerShelfID, wantStatus: http.StatusOK,
			wantCoreMethod: corev1.Forge_InsertPowerShelfHealthReport_FullMethodName,
		},
		{
			name: "remove PowerShelf",
			factory: func() echo.HandlerFunc {
				h := NewDeleteTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodDelete, query: "?siteId=" + fixture.SiteID + "&type=PowerShelf", source: "overrides.sre", resourceID: testPowerShelfID, wantStatus: http.StatusNoContent,
			wantCoreMethod: corev1.Forge_RemovePowerShelfHealthReport_FullMethodName,
		},
		{
			name: "reject missing type",
			factory: func() echo.HandlerFunc {
				h := NewGetAllTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodGet, query: "?siteId=" + fixture.SiteID, wantStatus: http.StatusBadRequest,
		},
		{
			name: "reject known mismatched ID namespace",
			factory: func() echo.HandlerFunc {
				h := NewGetAllTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodGet, query: "?siteId=" + fixture.SiteID + "&type=Compute", resourceID: testSwitchID, wantStatus: http.StatusBadRequest,
		},
		{
			name: "reject unauthorized request",
			factory: func() echo.HandlerFunc {
				h := NewGetAllTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
				return h.Handle
			},
			method: http.MethodGet, query: "?siteId=" + fixture.SiteID + "&type=Compute", wantStatus: http.StatusForbidden,
			user: unauthorizedUser,
		},
		{
			name: "hide internal dispatch error",
			factory: func() echo.HandlerFunc {
				h := newTrayHealthReportHandler(fixture.DBSession, fixture.SiteClientPool)
				return func(c echo.Context) error {
					return handleTrayHealthReport(c, h, trayHealthReportAction("Unknown"))
				}
			},
			method: http.MethodGet, query: "?siteId=" + fixture.SiteID + "&type=Compute", wantStatus: http.StatusInternalServerError,
			wantBodyExcludes: "unsupported Tray health report action",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			*fixture.ProxiedReq = grpcproxy.Request{}
			requestFixture := fixture
			if test.user != nil {
				requestFixture.User = test.user
			}
			if test.resourceID != "" {
				requestFixture.MachineID = test.resourceID
			}
			rec := requestFixture.Request(t, test.factory(), test.method, "/"+test.query, test.body, test.source)

			assert.Equal(t, test.wantStatus, rec.Code, rec.Body.String())
			assert.Equal(t, test.wantCoreMethod, fixture.ProxiedReq.FullMethod)
			if test.wantBodyExcludes != "" {
				assert.NotContains(t, rec.Body.String(), test.wantBodyExcludes)
			}
			if test.wantCoreMethod == "" {
				return
			}
			assert.Empty(t, fixture.ProxiedReq.EncryptedSecrets)
			wantSource := test.source
			if test.method == http.MethodPut {
				wantSource = "overrides.sre"
			}
			assertTrayHealthCoreRequest(t, fixture.ProxiedReq, requestFixture.MachineID, wantSource)
			assert.NotContains(t, rec.Body.String(), "password")
			if test.method == http.MethodGet && test.wantStatus == http.StatusOK {
				assert.Contains(t, rec.Body.String(), "overrides.sre")
			}
		})
	}
}

func TestValidateKnownTrayIDNamespace(t *testing.T) {
	tests := []struct {
		name       string
		resourceID string
		trayType   string
		wantError  string
	}{
		{name: "accept Compute namespace", resourceID: "fm100-rest-of-id", trayType: "Compute"},
		{name: "accept NVSwitch namespace", resourceID: "sw100-rest-of-id", trayType: "NVSwitch"},
		{name: "accept PowerShelf namespace", resourceID: "ps100-rest-of-id", trayType: "PowerShelf"},
		{name: "allow unknown namespace", resourceID: "future-rest-of-id", trayType: "Compute"},
		{name: "reject NVSwitch ID as Compute", resourceID: "sw100-rest-of-id", trayType: "Compute", wantError: "Tray ID namespace NVSwitch does not match type Compute"},
		{name: "reject Compute ID as NVSwitch", resourceID: "fm100-rest-of-id", trayType: "NVSwitch", wantError: "Tray ID namespace Compute does not match type NVSwitch"},
		{name: "reject NVSwitch ID as PowerShelf", resourceID: "sw100-rest-of-id", trayType: "PowerShelf", wantError: "Tray ID namespace NVSwitch does not match type PowerShelf"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := validateKnownTrayIDNamespace(test.resourceID, test.trayType)
			if test.wantError == "" {
				require.NoError(t, err)
				return
			}
			require.EqualError(t, err, test.wantError)
		})
	}
}

func TestTrayHealthReportCoreRequest(t *testing.T) {
	method, request, response, entry, err := trayHealthReportCoreRequest(trayHealthReportList, "CDU", "tray-1", "", model.APIMachineHealthReportEntryRequest{}, nil)

	require.EqualError(t, err, `unsupported Tray type "CDU"`)
	assert.Empty(t, method)
	assert.Nil(t, request)
	assert.Nil(t, response)
	assert.Nil(t, entry)
}

func validTrayHealthReportRequest(siteID, trayType string) model.APITrayHealthReportEntryRequest {
	return model.APITrayHealthReportEntryRequest{
		SiteID: siteID,
		Type:   trayType,
		APIMachineHealthReportEntryRequest: model.APIMachineHealthReportEntryRequest{
			Source:    "overrides.sre",
			Mode:      model.MachineHealthReportModeMerge,
			Successes: []model.APIMachineHealthProbeSuccess{{ID: "probe.ok"}},
		},
	}
}

func assertTrayHealthCoreRequest(t *testing.T, req *grpcproxy.Request, wantID, wantSource string) {
	t.Helper()

	var gotID, gotSource string
	switch req.FullMethod {
	case corev1.Forge_ListMachineHealthReports_FullMethodName:
		var value corev1.MachineId
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID = value.GetId()
	case corev1.Forge_InsertMachineHealthReport_FullMethodName:
		var value corev1.InsertMachineHealthReportRequest
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID, gotSource = value.GetMachineId().GetId(), value.GetHealthReportEntry().GetReport().GetSource()
	case corev1.Forge_RemoveMachineHealthReport_FullMethodName:
		var value corev1.RemoveMachineHealthReportRequest
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID, gotSource = value.GetMachineId().GetId(), value.GetSource()
	case corev1.Forge_ListSwitchHealthReports_FullMethodName:
		var value corev1.ListSwitchHealthReportsRequest
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID = value.GetSwitchId().GetId()
	case corev1.Forge_InsertSwitchHealthReport_FullMethodName:
		var value corev1.InsertSwitchHealthReportRequest
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID, gotSource = value.GetSwitchId().GetId(), value.GetHealthReportEntry().GetReport().GetSource()
	case corev1.Forge_RemoveSwitchHealthReport_FullMethodName:
		var value corev1.RemoveSwitchHealthReportRequest
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID, gotSource = value.GetSwitchId().GetId(), value.GetSource()
	case corev1.Forge_ListPowerShelfHealthReports_FullMethodName:
		var value corev1.ListPowerShelfHealthReportsRequest
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID = value.GetPowerShelfId().GetId()
	case corev1.Forge_InsertPowerShelfHealthReport_FullMethodName:
		var value corev1.InsertPowerShelfHealthReportRequest
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID, gotSource = value.GetPowerShelfId().GetId(), value.GetHealthReportEntry().GetReport().GetSource()
	case corev1.Forge_RemovePowerShelfHealthReport_FullMethodName:
		var value corev1.RemovePowerShelfHealthReportRequest
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &value))
		gotID, gotSource = value.GetPowerShelfId().GetId(), value.GetSource()
	default:
		require.FailNow(t, "unexpected Core method", req.FullMethod)
	}

	assert.Equal(t, wantID, gotID)
	assert.Equal(t, wantSource, gotSource)
}
