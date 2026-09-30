// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/google/uuid"
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
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestDpuPowerControlHandlerAcceptsGracefulRestart(t *testing.T) {
	f := newDpuPowerFixture(t, true)
	f.expect(&corev1.MachineIdList{MachineIds: []*corev1.MachineId{{Id: f.dpuID}}})
	f.expect(&corev1.MachineList{Machines: []*corev1.Machine{{
		Id: &corev1.MachineId{Id: f.dpuID}, MachineType: corev1.MachineType_DPU,
		Status: &corev1.MachineStatus{AssociatedHostMachineId: &corev1.MachineId{Id: f.hostID}},
	}}})
	f.expect(&corev1.AdminPowerControlResponse{Msg: cutil.GetPtr("accepted")})

	rec := f.request(t, model.APIDpuPowerControlRequest{
		Action: model.MachinePowerActionGracefulRestart, AcknowledgeAttachedInstance: cutil.GetPtr(true),
		ExpectedInstanceID: f.instanceID, ExpectedTenantID: f.tenantID,
	})
	require.Equal(t, http.StatusAccepted, rec.Code)
	require.Len(t, f.requests, 3)
	assert.Equal(t, corev1.Forge_AdminPowerControl_FullMethodName, f.requests[2].FullMethod)
	var request corev1.AdminPowerControlRequest
	require.NoError(t, protojson.Unmarshal(f.requests[2].RequestJSON, &request))
	assert.Equal(t, f.dpuID, request.GetMachineId())
	assert.Equal(t, corev1.AdminPowerControlRequest_GracefulRestart, request.GetAction())
}

func TestDpuPowerControlHandlerRequiresAttachedInstanceAcknowledgement(t *testing.T) {
	f := newDpuPowerFixture(t, true)
	f.expect(&corev1.MachineIdList{MachineIds: []*corev1.MachineId{{Id: f.dpuID}}})
	f.expect(&corev1.MachineList{Machines: []*corev1.Machine{{
		Id: &corev1.MachineId{Id: f.dpuID}, MachineType: corev1.MachineType_DPU,
		Status: &corev1.MachineStatus{AssociatedHostMachineId: &corev1.MachineId{Id: f.hostID}},
	}}})

	rec := f.request(t, model.APIDpuPowerControlRequest{Action: model.MachinePowerActionGracefulRestart})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Len(t, f.requests, 2)
}

func TestDpuPowerControlHandlerRejectsChangedAttachedInstance(t *testing.T) {
	f := newDpuPowerFixture(t, true)
	f.expect(&corev1.MachineIdList{MachineIds: []*corev1.MachineId{{Id: f.dpuID}}})
	f.expect(&corev1.MachineList{Machines: []*corev1.Machine{{
		Id: &corev1.MachineId{Id: f.dpuID}, MachineType: corev1.MachineType_DPU,
		Status: &corev1.MachineStatus{AssociatedHostMachineId: &corev1.MachineId{Id: f.hostID}},
	}}})

	rec := f.request(t, model.APIDpuPowerControlRequest{
		Action: model.MachinePowerActionGracefulRestart, AcknowledgeAttachedInstance: cutil.GetPtr(true),
		ExpectedInstanceID: uuid.NewString(), ExpectedTenantID: f.tenantID,
	})
	assert.Equal(t, http.StatusConflict, rec.Code)
	assert.Len(t, f.requests, 2)
}

func TestDpuPowerControlHandlerRejectsOtherPowerActionsBeforeDispatch(t *testing.T) {
	f := newDpuPowerFixture(t, false)
	rec := f.request(t, model.APIDpuPowerControlRequest{Action: model.MachinePowerActionForceRestart})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Empty(t, f.requests)
}

type dpuPowerFixture struct {
	org, siteID, hostID, dpuID, instanceID, tenantID string
	user                                             interface{}
	handler                                          DpuPowerControlHandler
	tsc                                              *tmocks.Client
	requests                                         []grpcproxy.Request
}

func newDpuPowerFixture(t *testing.T, assigned bool) *dpuPowerFixture {
	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)
	org := "test-org-" + uuid.NewString()
	user := common.TestBuildUser(t, dbSession, "test-starfleet-"+uuid.NewString(), org, []string{authz.ProviderAdminRole})
	provider := common.TestBuildInfrastructureProvider(t, dbSession, "provider", org, user)
	site := common.TestBuildSite(t, dbSession, provider, "site", user)
	_, err := cdbm.NewSiteDAO(dbSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: site.ID, Status: cutil.GetPtr(cdbm.SiteStatusRegistered)})
	require.NoError(t, err)
	host := common.TestBuildMachine(t, dbSession, provider, site, nil, cutil.GetPtr("host"), cdbm.MachineStatusReady)
	var instanceID, tenantID string
	if assigned {
		tenant := common.TestBuildTenant(t, dbSession, "tenant", org, user)
		instanceType := common.TestBuildInstanceType(t, dbSession, "instance-type", nil, site, nil, user)
		vpc := common.TestBuildVPC(t, dbSession, "vpc", provider, tenant, site, nil, nil, nil, cdbm.VpcStatusReady, user)
		operatingSystem := common.TestBuildOperatingSystem(t, dbSession, "os", tenant, cdbm.OperatingSystemStatusReady, user)
		instance := common.TestBuildInstance(t, dbSession, "instance", tenant.ID, provider.ID, site.ID,
			instanceType.ID, vpc.ID, &host.ID, operatingSystem.ID)
		instanceID, tenantID = instance.ID.String(), tenant.ID.String()
	}
	_, err = cdbm.NewMachineDAO(dbSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{MachineID: host.ID, IsAssigned: &assigned})
	require.NoError(t, err)
	tsc := &tmocks.Client{}
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = tsc
	return &dpuPowerFixture{
		org: org, siteID: site.ID.String(), hostID: host.ID, dpuID: "dpu-" + uuid.NewString(),
		instanceID: instanceID, tenantID: tenantID, user: user,
		handler: NewDpuPowerControlHandler(dbSession, pool), tsc: tsc,
	}
}

func (f *dpuPowerFixture) expect(response proto.Message) {
	run := &tmocks.WorkflowRun{}
	run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		out := args.Get(1).(*grpcproxy.Response)
		out.ResponseJSON, _ = protojson.Marshal(response)
	}).Return(nil).Once()
	f.tsc.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).
		Run(func(args mock.Arguments) { f.requests = append(f.requests, args.Get(3).(grpcproxy.Request)) }).Return(run, nil).Once()
}

func (f *dpuPowerFixture) request(t *testing.T, request model.APIDpuPowerControlRequest) *httptest.ResponseRecorder {
	body, err := json.Marshal(request)
	require.NoError(t, err)
	e := echo.New()
	req := httptest.NewRequest(http.MethodPatch, "/?siteId="+f.siteID, bytes.NewReader(body))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	c := e.NewContext(req, rec)
	c.SetParamNames("orgName", "id")
	c.SetParamValues(f.org, f.dpuID)
	c.Set("user", f.user)
	require.NoError(t, f.handler.Handle(c))
	return rec
}
