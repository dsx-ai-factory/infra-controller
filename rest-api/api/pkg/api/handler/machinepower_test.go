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

type machinePowerHandlerFixture struct {
	dbSession  *cdb.Session
	org        string
	machineID  string
	user       interface{}
	handler    echo.HandlerFunc
	proxiedReq *grpcproxy.Request
}

func newMachinePowerHandlerFixture(t *testing.T, response *corev1.AdminPowerControlResponse) machinePowerHandlerFixture {
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
	it := common.TestBuildInstanceType(t, dbSession, "test-instance-type", cutil.GetPtr(site.ID), site, nil, user)
	machine := common.TestBuildMachine(t, dbSession, ip, site, &it.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)

	proxiedReq := &grpcproxy.Request{}
	wrun := &tmocks.WorkflowRun{}
	wrun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		if response == nil {
			return
		}
		out := args.Get(1).(*grpcproxy.Response)
		respJSON, err := protojson.Marshal(response)
		require.NoError(t, err)
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

	h := NewMachinePowerControlHandler(dbSession, scp, common.GetTestConfig())
	return machinePowerHandlerFixture{
		dbSession:  dbSession,
		org:        org,
		machineID:  machine.ID,
		user:       user,
		handler:    h.Handle,
		proxiedReq: proxiedReq,
	}
}

func (f machinePowerHandlerFixture) request(t *testing.T, method, target string, body any) *httptest.ResponseRecorder {
	t.Helper()

	var reqBody string
	if body != nil {
		bodyBytes, err := json.Marshal(body)
		require.NoError(t, err)
		reqBody = string(bodyBytes)
	}

	e := echo.New()
	req := httptest.NewRequest(method, target, strings.NewReader(reqBody))
	if body != nil {
		req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	}
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName", "id")
	ec.SetParamValues(f.org, f.machineID)
	ec.Set("user", f.user)

	require.NoError(t, f.handler(ec))
	return rec
}

func TestMachinePowerControlHandlerProxiesRequest(t *testing.T) {
	msg := "power control accepted"
	fixture := newMachinePowerHandlerFixture(t, &corev1.AdminPowerControlResponse{Msg: &msg})

	rec := fixture.request(t, http.MethodPatch, "/", model.APIMachinePowerControlRequest{Action: model.MachinePowerActionForceRestart})
	assert.Equal(t, http.StatusAccepted, rec.Code)
	assert.Equal(t, corev1.Forge_AdminPowerControl_FullMethodName, fixture.proxiedReq.FullMethod)
	assert.Empty(t, fixture.proxiedReq.EncryptedSecrets)

	var coreReq corev1.AdminPowerControlRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreReq))
	assert.Equal(t, fixture.machineID, coreReq.GetMachineId())
	assert.Equal(t, corev1.AdminPowerControlRequest_ForceRestart, coreReq.GetAction())

	var apiResp model.APIMessageResponse
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiResp))
	assert.Equal(t, "power control accepted", apiResp.Message)
	assert.NotContains(t, rec.Body.String(), "password")
}

func TestMachinePowerControlHandlerRejectsInvalidAction(t *testing.T) {
	fixture := newMachinePowerHandlerFixture(t, nil)

	rec := fixture.request(t, http.MethodPatch, "/", model.APIMachinePowerControlRequest{Action: "forcecycle"})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestMachinePowerControlHandlerRequiresProviderAdmin(t *testing.T) {
	fixture := newMachinePowerHandlerFixture(t, nil)
	fixture.user = &cdbm.User{OrgData: cdbm.OrgData{fixture.org: cdbm.Org{Name: fixture.org}}}

	rec := fixture.request(t, http.MethodPatch, "/", model.APIMachinePowerControlRequest{Action: model.MachinePowerActionOn})
	assert.Equal(t, http.StatusForbidden, rec.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestMachinePowerControlHandlerRejectsProviderViewer(t *testing.T) {
	fixture := newMachinePowerHandlerFixture(t, nil)
	fixture.user = &cdbm.User{OrgData: cdbm.OrgData{fixture.org: cdbm.Org{
		Name:  fixture.org,
		Roles: []string{authz.ProviderViewerRole},
	}}}

	rec := fixture.request(t, http.MethodPatch, "/", model.APIMachinePowerControlRequest{Action: model.MachinePowerActionOn})
	assert.Equal(t, http.StatusForbidden, rec.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestMachinePowerControlHandlerRejectsUnknownMachine(t *testing.T) {
	fixture := newMachinePowerHandlerFixture(t, nil)
	fixture.machineID = "missing-machine"

	rec := fixture.request(t, http.MethodPatch, "/", model.APIMachinePowerControlRequest{Action: model.MachinePowerActionOn})
	assert.Equal(t, http.StatusNotFound, rec.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestMachinePowerControlHandlerHidesForeignMachine(t *testing.T) {
	fixture := newMachinePowerHandlerFixture(t, nil)
	otherOrg := "other-org"
	otherUser := common.TestBuildUser(t, fixture.dbSession, "other-starfleet-id", otherOrg, []string{authz.ProviderAdminRole})
	otherProvider := common.TestBuildInfrastructureProvider(t, fixture.dbSession, "Other Infrastructure Provider", otherOrg, otherUser)
	otherSite := common.TestBuildSite(t, fixture.dbSession, otherProvider, "Other Site", otherUser)
	otherIT := common.TestBuildInstanceType(t, fixture.dbSession, "other-instance-type", cutil.GetPtr(otherSite.ID), otherSite, nil, otherUser)
	otherMachine := common.TestBuildMachine(t, fixture.dbSession, otherProvider, otherSite, &otherIT.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)
	fixture.machineID = otherMachine.ID

	rec := fixture.request(t, http.MethodPatch, "/", model.APIMachinePowerControlRequest{Action: model.MachinePowerActionOn})
	assert.Equal(t, http.StatusNotFound, rec.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestMachinePowerControlHandlerRejectsMachineWithInstanceNoAcknowledgement(t *testing.T) {
	fixture := newMachinePowerHandlerFixture(t, nil)
	_, err := cdbm.NewMachineDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{
		MachineID:  fixture.machineID,
		IsAssigned: cutil.GetPtr(true),
	})
	require.NoError(t, err)

	rec := fixture.request(t, http.MethodPatch, "/", model.APIMachinePowerControlRequest{Action: model.MachinePowerActionOn})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestMachinePowerControlHandlerRejectsMachineWithInstanceWithAcknowledgement(t *testing.T) {
	fixture := newMachinePowerHandlerFixture(t, nil)
	_, err := cdbm.NewMachineDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{
		MachineID:  fixture.machineID,
		IsAssigned: cutil.GetPtr(true),
	})
	require.NoError(t, err)

	rec := fixture.request(t, http.MethodPatch, "/", model.APIMachinePowerControlRequest{Action: model.MachinePowerActionOn, AcknowledgeAttachedInstance: cutil.GetPtr(true)})
	assert.Equal(t, http.StatusAccepted, rec.Code)
	assert.Equal(t, corev1.Forge_AdminPowerControl_FullMethodName, fixture.proxiedReq.FullMethod)
}

func TestMachinePowerControlHandlerRejectsEmptyMachineID(t *testing.T) {
	fixture := newMachinePowerHandlerFixture(t, nil)
	fixture.machineID = ""

	rec := fixture.request(t, http.MethodPatch, "/", model.APIMachinePowerControlRequest{Action: model.MachinePowerActionOn})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}
