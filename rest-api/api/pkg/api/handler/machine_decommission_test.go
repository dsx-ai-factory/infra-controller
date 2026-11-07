// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
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

type decommissionMachineFixture struct {
	dbSession  *cdb.Session
	org        string
	machineID  string
	user       interface{}
	handler    echo.HandlerFunc
	proxiedReq *grpcproxy.Request
}

func newDecommissionMachineFixture(t *testing.T) decommissionMachineFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	org := "test-org"
	user := common.TestBuildUser(t, dbSession, "test-starfleet-id", org, []string{authz.ProviderAdminRole})
	provider := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, user)
	site := common.TestBuildSite(t, dbSession, provider, "Test Site", user)
	_, err := cdbm.NewSiteDAO(dbSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)
	instanceType := common.TestBuildInstanceType(t, dbSession, "test-instance-type", cutil.GetPtr(site.ID), site, nil, user)
	machine := common.TestBuildMachine(t, dbSession, provider, site, &instanceType.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)

	proxiedReq := &grpcproxy.Request{}
	workflowRun := &tmocks.WorkflowRun{}
	workflowRun.On("Get", mock.Anything, mock.Anything).Return(nil)
	temporalClient := &tmocks.Client{}
	temporalClient.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(request grpcproxy.Request) bool {
			*proxiedReq = request
			return true
		}),
	).Return(workflowRun, nil)

	clientPool := sc.NewClientPool(nil)
	clientPool.IDClientMap[site.ID.String()] = temporalClient
	handler := NewDecommissionMachineHandler(dbSession, clientPool, common.GetTestConfig())

	return decommissionMachineFixture{
		dbSession:  dbSession,
		org:        org,
		machineID:  machine.ID,
		user:       user,
		handler:    handler.Handle,
		proxiedReq: proxiedReq,
	}
}

func (f decommissionMachineFixture) request(t *testing.T, method string, handler echo.HandlerFunc) *httptest.ResponseRecorder {
	t.Helper()

	echoServer := echo.New()
	request := httptest.NewRequest(method, "/", nil)
	recorder := httptest.NewRecorder()
	context := echoServer.NewContext(request, recorder)
	context.SetParamNames("orgName", "id")
	context.SetParamValues(f.org, f.machineID)
	context.Set("user", f.user)
	require.NoError(t, handler(context))
	return recorder
}

func TestDecommissionMachineHandlerProxiesRequest(t *testing.T) {
	fixture := newDecommissionMachineFixture(t)

	recorder := fixture.request(t, http.MethodPost, fixture.handler)
	assert.Equal(t, http.StatusAccepted, recorder.Code)
	assert.Equal(t, corev1.Forge_DecommissionManagedHost_FullMethodName, fixture.proxiedReq.FullMethod)

	var coreRequest corev1.DecommissionManagedHostRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreRequest))
	assert.Equal(t, fixture.machineID, coreRequest.GetMachineId().GetId())

	var response model.APIMessageResponse
	require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
	assert.Equal(t, "Machine decommissioning request was accepted", response.Message)
}

func TestDecommissionMachineHandlerRejectsProviderViewer(t *testing.T) {
	fixture := newDecommissionMachineFixture(t)
	fixture.user = &cdbm.User{OrgData: cdbm.OrgData{fixture.org: cdbm.Org{
		Name:  fixture.org,
		Roles: []string{authz.ProviderViewerRole},
	}}}

	recorder := fixture.request(t, http.MethodPost, fixture.handler)
	assert.Equal(t, http.StatusForbidden, recorder.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestDecommissionMachineHandlerRejectsUnknownMachine(t *testing.T) {
	fixture := newDecommissionMachineFixture(t)
	fixture.machineID = "missing-machine"

	recorder := fixture.request(t, http.MethodPost, fixture.handler)
	assert.Equal(t, http.StatusNotFound, recorder.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestDecommissionMachineHandlerRejectsMissingMachine(t *testing.T) {
	fixture := newDecommissionMachineFixture(t)
	_, err := cdbm.NewMachineDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{
		MachineID:       fixture.machineID,
		IsMissingOnSite: cutil.GetPtr(true),
	})
	require.NoError(t, err)

	recorder := fixture.request(t, http.MethodPost, fixture.handler)
	assert.Equal(t, http.StatusBadRequest, recorder.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}
