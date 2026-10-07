// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
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
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

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

func TestRotateCredentialHandlerProxiesRequest(t *testing.T) {
	started := timestamppb.Now()
	fixture := newCredentialRotationHandlerFixture(t, &corev1.RotateCredentialResult{
		CredentialType: corev1.RotationCredentialType_ROTATION_BMC,
		TargetVersion:  7,
		StartedAt:      started,
	})
	handler := NewRotateCredentialHandler(fixture.dbSession, fixture.scp)

	rec := fixture.post(t, handler.Handle, model.APICredentialRotationRequest{
		SiteID:         fixture.siteID,
		CredentialType: model.CredentialRotationTypeBMC,
		Password:       cutil.GetPtr("secret-password"),
		Reason:         cutil.GetPtr("annual rotation"),
	})

	assert.Equal(t, http.StatusOK, rec.Code)
	assert.Equal(t, corev1.Forge_RotateCredential_FullMethodName, fixture.proxiedReq.FullMethod)
	assert.NotContains(t, string(fixture.proxiedReq.RequestJSON), "secret-password")
	assert.NotEmpty(t, fixture.proxiedReq.EncryptedSecrets)

	var coreReq corev1.RotateCredentialRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreReq))
	assert.Equal(t, corev1.RotationCredentialType_ROTATION_BMC, coreReq.GetCredentialType())
	assert.Equal(t, grpcproxy.RedactedPlaceholder, coreReq.GetPassword())
	assert.Equal(t, "annual rotation", coreReq.GetReason())

	var resp model.APICredentialRotationResult
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
	assert.Equal(t, model.CredentialRotationTypeBMC, resp.CredentialType)
	assert.Equal(t, uint32(7), resp.TargetVersion)
	assert.NotContains(t, rec.Body.String(), "password")
}

func TestRotateCredentialHandlerRejectsInvalidRequest(t *testing.T) {
	fixture := newCredentialRotationHandlerFixture(t, nil)
	handler := NewRotateCredentialHandler(fixture.dbSession, fixture.scp)

	rec := fixture.post(t, handler.Handle, model.APICredentialRotationRequest{
		SiteID: fixture.siteID,
		// credentialType omitted.
	})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestGetCredentialRotationStatusHandlerProxiesRequest(t *testing.T) {
	fixture := newCredentialRotationHandlerFixture(t, &corev1.CredentialRotationStatusResult{
		TargetVersion: 3,
		Converged:     5,
		Pending:       2,
		Complete:      false,
	})
	handler := NewGetCredentialRotationStatusHandler(fixture.dbSession, fixture.scp)

	rec := fixture.get(t, handler.Handle, url.Values{
		"siteId":         {fixture.siteID},
		"credentialType": {string(model.CredentialRotationTypeBMC)},
	})

	assert.Equal(t, http.StatusOK, rec.Code)
	assert.Equal(t, corev1.Forge_GetCredentialRotationStatus_FullMethodName, fixture.proxiedReq.FullMethod)
	assert.Empty(t, fixture.proxiedReq.EncryptedSecrets)

	var coreReq corev1.CredentialRotationStatusRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreReq))
	assert.Equal(t, corev1.RotationCredentialType_ROTATION_BMC, coreReq.GetCredentialType())
	assert.Empty(t, coreReq.GetDeviceMac())

	var resp model.APICredentialRotationStatus
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
	assert.Equal(t, uint32(3), resp.TargetVersion)
	assert.Equal(t, uint64(5), resp.Converged)
	assert.Equal(t, uint64(2), resp.Pending)
}

func TestGetCredentialRotationStatusHandlerTargetsDevice(t *testing.T) {
	fixture := newCredentialRotationHandlerFixture(t, &corev1.CredentialRotationStatusResult{
		TargetVersion: 3,
		Device:        &corev1.DeviceCredentialRotationStatus{DeviceMac: "aa:bb:cc:dd:ee:ff"},
	})
	handler := NewGetCredentialRotationStatusHandler(fixture.dbSession, fixture.scp)

	rec := fixture.get(t, handler.Handle, url.Values{
		"siteId":         {fixture.siteID},
		"credentialType": {string(model.CredentialRotationTypeBMC)},
		"deviceMac":      {"aa:bb:cc:dd:ee:ff"},
	})

	assert.Equal(t, http.StatusOK, rec.Code)
	var coreReq corev1.CredentialRotationStatusRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreReq))
	assert.Equal(t, "aa:bb:cc:dd:ee:ff", coreReq.GetDeviceMac())

	var resp model.APICredentialRotationStatus
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
	require.NotNil(t, resp.Device)
	assert.Equal(t, "aa:bb:cc:dd:ee:ff", resp.Device.DeviceMac)
}

func TestGetCredentialRotationStatusHandlerRejectsInvalidQuery(t *testing.T) {
	cases := []struct {
		name  string
		query url.Values
	}{
		{"missing siteId", url.Values{"credentialType": {string(model.CredentialRotationTypeBMC)}}},
		{"invalid siteId", url.Values{"siteId": {"not-a-uuid"}, "credentialType": {string(model.CredentialRotationTypeBMC)}}},
		{"missing credentialType", url.Values{"siteId": {uuidString(t)}}},
		{"invalid credentialType", url.Values{"siteId": {uuidString(t)}, "credentialType": {"nope"}}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			fixture := newCredentialRotationHandlerFixture(t, nil)
			handler := NewGetCredentialRotationStatusHandler(fixture.dbSession, fixture.scp)
			rec := fixture.get(t, handler.Handle, tc.query)
			assert.Equal(t, http.StatusBadRequest, rec.Code)
			assert.Empty(t, fixture.proxiedReq.FullMethod)
		})
	}
}

type credentialRotationHandlerFixture struct {
	org        string
	siteID     string
	user       interface{}
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	proxiedReq *grpcproxy.Request
}

func newCredentialRotationHandlerFixture(t *testing.T, response proto.Message) credentialRotationHandlerFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	org := "test-org"
	user := common.TestBuildUser(t, dbSession, "test-starfleet-id", org, []string{authz.ProviderAdminRole})
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, user)
	site := common.TestBuildSite(t, dbSession, ip, "Test Site", user)
	sDAO := cdbm.NewSiteDAO(dbSession)
	_, err := sDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)

	proxiedReq := &grpcproxy.Request{}
	wrun := &tmocks.WorkflowRun{}
	wrun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		if response == nil {
			return
		}
		out := args.Get(1).(*grpcproxy.Response)
		respJSON, merr := protojson.Marshal(response)
		require.NoError(t, merr)
		out.ResponseJSON = respJSON
	}).Return(nil)

	tsc := &tmocks.Client{}
	tsc.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool {
			*proxiedReq = req
			return true
		}),
	).Return(wrun, nil)

	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = tsc

	return credentialRotationHandlerFixture{
		org:        org,
		siteID:     site.ID.String(),
		user:       user,
		dbSession:  dbSession,
		scp:        scp,
		proxiedReq: proxiedReq,
	}
}

func (f credentialRotationHandlerFixture) post(t *testing.T, handle echo.HandlerFunc, apiReq model.APICredentialRotationRequest) *httptest.ResponseRecorder {
	t.Helper()

	body, err := json.Marshal(apiReq)
	require.NoError(t, err)

	e := echo.New()
	req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(body)))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName")
	ec.SetParamValues(f.org)
	ec.Set("user", f.user)

	require.NoError(t, handle(ec))
	return rec
}

func (f credentialRotationHandlerFixture) get(t *testing.T, handle echo.HandlerFunc, query url.Values) *httptest.ResponseRecorder {
	t.Helper()

	e := echo.New()
	req := httptest.NewRequest(http.MethodGet, "/?"+query.Encode(), nil)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName")
	ec.SetParamValues(f.org)
	ec.Set("user", f.user)

	require.NoError(t, handle(ec))
	return rec
}

func uuidString(t *testing.T) string {
	t.Helper()
	return uuid.NewString()
}
