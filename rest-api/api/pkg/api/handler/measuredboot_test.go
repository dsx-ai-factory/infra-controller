// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestCreateMeasuredBootTrustedMachineHandler(t *testing.T) {
	record := &corev1.MeasurementApprovedMachineRecordPb{
		ApprovalId:   &corev1.MeasurementApprovedMachineId{Value: "00000000-0000-0000-0000-000000000010"},
		MachineId:    "*",
		ApprovalType: corev1.MeasurementApprovedTypePb_Persist,
	}
	fixture := newMeasuredBootHandlerFixture(t, &corev1.AddMeasurementTrustedMachineResponse{ApprovalRecord: record}, nil)
	handler := NewCreateMeasuredBootTrustedMachineHandler(fixture.dbSession, fixture.scp)

	rec := fixture.request(t, handler.Handle, http.MethodPost, "/", "", model.APIMeasuredBootTrustedMachineCreateRequest{
		SiteID:       fixture.siteID,
		MachineID:    "*",
		ApprovalType: model.MeasuredBootApprovalTypePersist,
	})
	assert.Equal(t, http.StatusCreated, rec.Code)
	assert.Equal(t, corev1.Forge_AddMeasurementTrustedMachine_FullMethodName, fixture.proxiedReq.FullMethod)

	var coreReq corev1.AddMeasurementTrustedMachineRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreReq))
	assert.Equal(t, "*", coreReq.GetMachineId())
	assert.Equal(t, corev1.MeasurementApprovedTypePb_Persist, coreReq.GetApprovalType())

	var resp model.APIMeasuredBootTrustedMachine
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
	assert.Equal(t, record.GetApprovalId().GetValue(), resp.ApprovalID)
}

func TestGetAllMeasuredBootTrustedMachineHandler(t *testing.T) {
	record := &corev1.MeasurementApprovedMachineRecordPb{
		ApprovalId: &corev1.MeasurementApprovedMachineId{Value: "00000000-0000-0000-0000-000000000010"},
		MachineId:  "00000000-0000-0000-0000-000000000011",
	}
	fixture := newMeasuredBootHandlerFixture(t, &corev1.ListMeasurementTrustedMachinesResponse{ApprovalRecords: []*corev1.MeasurementApprovedMachineRecordPb{record}}, nil)
	handler := NewGetAllMeasuredBootTrustedMachineHandler(fixture.dbSession, fixture.scp)

	rec := fixture.request(t, handler.Handle, http.MethodGet, "/?siteId="+fixture.siteID, "", nil)
	assert.Equal(t, http.StatusOK, rec.Code)
	assert.Equal(t, corev1.Forge_ListMeasurementTrustedMachines_FullMethodName, fixture.proxiedReq.FullMethod)

	var resp []*model.APIMeasuredBootTrustedMachine
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
	require.Len(t, resp, 1)
	assert.Equal(t, record.GetMachineId(), resp[0].MachineID)
}

func TestDeleteMeasuredBootTrustedMachineHandler(t *testing.T) {
	record := &corev1.MeasurementApprovedMachineRecordPb{
		ApprovalId: &corev1.MeasurementApprovedMachineId{Value: "00000000-0000-0000-0000-000000000010"},
		MachineId:  "00000000-0000-0000-0000-000000000011",
	}
	fixture := newMeasuredBootHandlerFixture(t, &corev1.RemoveMeasurementTrustedMachineResponse{ApprovalRecord: record}, nil)
	handler := NewDeleteMeasuredBootTrustedMachineHandler(fixture.dbSession, fixture.scp)

	target := "/?siteId=" + fixture.siteID + "&selector=" + model.MeasuredBootTrustedMachineSelectorMachineID
	rec := fixture.request(t, handler.Handle, http.MethodDelete, target, record.GetMachineId(), nil)
	assert.Equal(t, http.StatusOK, rec.Code)
	assert.Equal(t, corev1.Forge_RemoveMeasurementTrustedMachine_FullMethodName, fixture.proxiedReq.FullMethod)

	var coreReq corev1.RemoveMeasurementTrustedMachineRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreReq))
	assert.Equal(t, record.GetMachineId(), coreReq.GetMachineId())
}

func TestCreateMeasuredBootTrustedProfileHandler(t *testing.T) {
	record := &corev1.MeasurementApprovedProfileRecordPb{
		ApprovalId: &corev1.MeasurementApprovedProfileId{Value: "00000000-0000-0000-0000-000000000010"},
		ProfileId:  &corev1.MeasurementSystemProfileId{Value: "00000000-0000-0000-0000-000000000012"},
	}
	fixture := newMeasuredBootHandlerFixture(t, &corev1.AddMeasurementTrustedProfileResponse{ApprovalRecord: record}, nil)
	handler := NewCreateMeasuredBootTrustedProfileHandler(fixture.dbSession, fixture.scp)

	rec := fixture.request(t, handler.Handle, http.MethodPost, "/", "", model.APIMeasuredBootTrustedProfileCreateRequest{
		SiteID:       fixture.siteID,
		ProfileID:    record.GetProfileId().GetValue(),
		ApprovalType: model.MeasuredBootApprovalTypeOneshot,
	})
	assert.Equal(t, http.StatusCreated, rec.Code)
	assert.Equal(t, corev1.Forge_AddMeasurementTrustedProfile_FullMethodName, fixture.proxiedReq.FullMethod)
}

func TestGetAllMeasuredBootTrustedProfileHandler(t *testing.T) {
	record := &corev1.MeasurementApprovedProfileRecordPb{
		ApprovalId: &corev1.MeasurementApprovedProfileId{Value: "00000000-0000-0000-0000-000000000010"},
		ProfileId:  &corev1.MeasurementSystemProfileId{Value: "00000000-0000-0000-0000-000000000012"},
	}
	fixture := newMeasuredBootHandlerFixture(t, &corev1.ListMeasurementTrustedProfilesResponse{ApprovalRecords: []*corev1.MeasurementApprovedProfileRecordPb{record}}, nil)
	handler := NewGetAllMeasuredBootTrustedProfileHandler(fixture.dbSession, fixture.scp)

	rec := fixture.request(t, handler.Handle, http.MethodGet, "/?siteId="+fixture.siteID, "", nil)
	assert.Equal(t, http.StatusOK, rec.Code)
	assert.Equal(t, corev1.Forge_ListMeasurementTrustedProfiles_FullMethodName, fixture.proxiedReq.FullMethod)
}

func TestDeleteMeasuredBootTrustedProfileHandler(t *testing.T) {
	record := &corev1.MeasurementApprovedProfileRecordPb{
		ApprovalId: &corev1.MeasurementApprovedProfileId{Value: "00000000-0000-0000-0000-000000000010"},
		ProfileId:  &corev1.MeasurementSystemProfileId{Value: "00000000-0000-0000-0000-000000000012"},
	}
	fixture := newMeasuredBootHandlerFixture(t, &corev1.RemoveMeasurementTrustedProfileResponse{ApprovalRecord: record}, nil)
	handler := NewDeleteMeasuredBootTrustedProfileHandler(fixture.dbSession, fixture.scp)

	target := "/?siteId=" + fixture.siteID + "&selector=" + model.MeasuredBootTrustedProfileSelectorApprovalID
	rec := fixture.request(t, handler.Handle, http.MethodDelete, target, record.GetApprovalId().GetValue(), nil)
	assert.Equal(t, http.StatusOK, rec.Code)
	assert.Equal(t, corev1.Forge_RemoveMeasurementTrustedProfile_FullMethodName, fixture.proxiedReq.FullMethod)
}

func TestMeasuredBootHandlersRejectInvalidInput(t *testing.T) {
	fixture := newMeasuredBootHandlerFixture(t, nil, nil)
	createMachineHandler := NewCreateMeasuredBootTrustedMachineHandler(fixture.dbSession, fixture.scp)
	createProfileHandler := NewCreateMeasuredBootTrustedProfileHandler(fixture.dbSession, fixture.scp)

	tests := []struct {
		name    string
		handler func(echo.Context) error
		body    any
		message string
	}{
		{
			name:    "malformed machine request",
			handler: createMachineHandler.Handle,
			body:    "invalid",
			message: "Failed to parse request data, potentially invalid structure",
		},
		{
			name:    "malformed profile request",
			handler: createProfileHandler.Handle,
			body:    "invalid",
			message: "Failed to parse request data, potentially invalid structure",
		},
		{
			name:    "invalid machine request",
			handler: createMachineHandler.Handle,
			body: model.APIMeasuredBootTrustedMachineCreateRequest{
				SiteID:       fixture.siteID,
				MachineID:    "invalid",
				ApprovalType: model.MeasuredBootApprovalTypeOneshot,
			},
			message: "Error validating Measured Boot Trusted Machine creation request data",
		},
		{
			name:    "invalid profile request",
			handler: createProfileHandler.Handle,
			body: model.APIMeasuredBootTrustedProfileCreateRequest{
				SiteID:       fixture.siteID,
				ProfileID:    "invalid",
				ApprovalType: model.MeasuredBootApprovalTypeOneshot,
			},
			message: "Error validating Measured Boot Trusted Profile creation request data",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := fixture.request(t, tt.handler, http.MethodPost, "/", "", tt.body)
			assert.Equal(t, http.StatusBadRequest, rec.Code)

			var apiErr struct {
				Message string `json:"message"`
			}
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiErr))
			assert.Equal(t, tt.message, apiErr.Message)
		})
	}

	deleteTests := []struct {
		name    string
		handler func(echo.Context) error
		message string
	}{
		{
			name:    "invalid machine deletion request",
			handler: NewDeleteMeasuredBootTrustedMachineHandler(fixture.dbSession, fixture.scp).Handle,
			message: "Error validating Measured Boot Trusted Machine deletion request data",
		},
		{
			name:    "invalid profile deletion request",
			handler: NewDeleteMeasuredBootTrustedProfileHandler(fixture.dbSession, fixture.scp).Handle,
			message: "Error validating Measured Boot Trusted Profile deletion request data",
		},
	}

	for _, tt := range deleteTests {
		t.Run(tt.name, func(t *testing.T) {
			rec := fixture.request(t, tt.handler, http.MethodDelete, "/?siteId="+fixture.siteID+"&selector=invalid", "invalid", nil)
			assert.Equal(t, http.StatusBadRequest, rec.Code)

			var apiErr struct {
				Message string `json:"message"`
			}
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiErr))
			assert.Equal(t, tt.message, apiErr.Message)
		})
	}
}

func TestMeasuredBootHandlerRequiresProviderAdmin(t *testing.T) {
	fixture := newMeasuredBootHandlerFixture(t, nil, []string{authz.TenantAdminRole})
	listHandler := NewGetAllMeasuredBootTrustedMachineHandler(fixture.dbSession, fixture.scp)

	rec := fixture.request(t, listHandler.Handle, http.MethodGet, "/?siteId="+fixture.siteID, "", nil)
	assert.Equal(t, http.StatusForbidden, rec.Code)

	deleteHandler := NewDeleteMeasuredBootTrustedMachineHandler(fixture.dbSession, fixture.scp)
	rec = fixture.request(t, deleteHandler.Handle, http.MethodDelete, "/?siteId="+fixture.siteID+"&selector=invalid", "invalid", nil)
	assert.Equal(t, http.StatusBadRequest, rec.Code)

	rec = fixture.request(t, deleteHandler.Handle, http.MethodDelete, "/?siteId="+fixture.siteID+"&selector="+model.MeasuredBootTrustedMachineSelectorMachineID, "00000000-0000-0000-0000-000000000011", nil)
	assert.Equal(t, http.StatusForbidden, rec.Code)
}

type measuredBootHandlerFixture struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	org        string
	siteID     string
	user       *cdbm.User
	proxiedReq *grpcproxy.Request
}

func newMeasuredBootHandlerFixture(t *testing.T, response proto.Message, roles []string) measuredBootHandlerFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	if roles == nil {
		roles = []string{authz.ProviderAdminRole}
	}
	org := "test-org"
	user := common.TestBuildUser(t, dbSession, "test-starfleet-id", org, roles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, user)
	site := common.TestBuildSite(t, dbSession, ip, "Test Site", user)
	sDAO := cdbm.NewSiteDAO(dbSession)
	_, err := sDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: site.ID, Status: cutil.GetPtr(cdbm.SiteStatusRegistered)})
	require.NoError(t, err)

	proxiedReq := &grpcproxy.Request{}
	wrun := &tmocks.WorkflowRun{}
	wrun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		if response == nil {
			return
		}
		responseJSON, err := protojson.Marshal(response)
		require.NoError(t, err)
		args.Get(1).(*grpcproxy.Response).ResponseJSON = responseJSON
	}).Return(nil)

	tsc := &tmocks.Client{}
	tsc.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.MatchedBy(func(req grpcproxy.Request) bool {
		*proxiedReq = req
		return true
	})).Return(wrun, nil)

	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = tsc

	return measuredBootHandlerFixture{
		dbSession: dbSession, scp: scp, org: org, siteID: site.ID.String(), user: user, proxiedReq: proxiedReq,
	}
}

func (f measuredBootHandlerFixture) request(t *testing.T, handler func(echo.Context) error, method, target, id string, body any) *httptest.ResponseRecorder {
	t.Helper()

	var requestBody string
	if body != nil {
		data, err := json.Marshal(body)
		require.NoError(t, err)
		requestBody = string(data)
	}
	req := httptest.NewRequest(method, target, strings.NewReader(requestBody))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	e := echo.New()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName", "id")
	ec.SetParamValues(f.org, id)
	ec.Set("user", f.user)

	require.NoError(t, handler(ec))
	return rec
}
