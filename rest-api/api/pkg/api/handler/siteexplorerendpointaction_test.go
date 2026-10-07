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

func TestSiteExplorerEndpointActionHandlerClearErrorForSelectedEndpoints(t *testing.T) {
	fixture := newSiteExplorerEndpointActionHandlerFixture(t, []string{authz.ProviderAdminRole})
	fixture.expectProxyResponse(t, nil)

	rec := fixture.request(t, model.APISiteExplorerEndpointActionRequest{
		SiteID:      fixture.siteID,
		Action:      model.SiteExplorerEndpointActionClearError,
		Target:      model.SiteExplorerEndpointTargetEndpointIDs,
		EndpointIDs: []string{"10.0.0.1"},
	})
	assert.Equal(t, http.StatusOK, rec.Code)
	require.Len(t, fixture.proxiedReqs, 1)
	assert.Equal(t, corev1.Forge_ClearSiteExplorationError_FullMethodName, fixture.proxiedReqs[0].FullMethod)
	assert.Empty(t, fixture.proxiedReqs[0].EncryptedSecrets)

	var coreReq corev1.ClearSiteExplorationErrorRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[0].RequestJSON, &coreReq))
	assert.Equal(t, "10.0.0.1", coreReq.GetIpAddress())

	var resp model.APISiteExplorerEndpointAction
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
	assert.Equal(t, fixture.siteID, resp.SiteID)
	assert.Equal(t, []string{"10.0.0.1"}, resp.EndpointIDs)
}

func TestSiteExplorerEndpointActionHandlerReExploreForAllEndpoints(t *testing.T) {
	fixture := newSiteExplorerEndpointActionHandlerFixture(t, []string{authz.ProviderAdminRole})
	fixture.expectProxyResponse(t, &corev1.ExploredEndpointIdList{EndpointIds: []string{"10.0.0.1", "10.0.0.2"}})
	fixture.expectProxyResponse(t, nil)
	fixture.expectProxyResponse(t, nil)

	rec := fixture.request(t, model.APISiteExplorerEndpointActionRequest{
		SiteID: fixture.siteID,
		Action: model.SiteExplorerEndpointActionReExplore,
		Target: model.SiteExplorerEndpointTargetAll,
	})
	assert.Equal(t, http.StatusOK, rec.Code)
	require.Len(t, fixture.proxiedReqs, 3)
	assert.Equal(t, corev1.Forge_FindExploredEndpointIds_FullMethodName, fixture.proxiedReqs[0].FullMethod)
	assert.Equal(t, corev1.Forge_ReExploreEndpoint_FullMethodName, fixture.proxiedReqs[1].FullMethod)
	assert.Equal(t, corev1.Forge_ReExploreEndpoint_FullMethodName, fixture.proxiedReqs[2].FullMethod)

	var firstAction corev1.ReExploreEndpointRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[1].RequestJSON, &firstAction))
	assert.Equal(t, "10.0.0.1", firstAction.GetIpAddress())
	var secondAction corev1.ReExploreEndpointRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[2].RequestJSON, &secondAction))
	assert.Equal(t, "10.0.0.2", secondAction.GetIpAddress())
}

func TestSiteExplorerEndpointActionHandlerRejectsInvalidRequest(t *testing.T) {
	fixture := newSiteExplorerEndpointActionHandlerFixture(t, []string{authz.ProviderAdminRole})

	rec := fixture.request(t, model.APISiteExplorerEndpointActionRequest{
		SiteID:      fixture.siteID,
		Action:      model.SiteExplorerEndpointActionClearError,
		Target:      model.SiteExplorerEndpointTargetEndpointIDs,
		EndpointIDs: []string{"not-an-ip"},
	})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Empty(t, fixture.proxiedReqs)
}

func TestSiteExplorerEndpointActionHandlerRejectsNonProviderAdmin(t *testing.T) {
	fixture := newSiteExplorerEndpointActionHandlerFixture(t, nil)

	rec := fixture.request(t, model.APISiteExplorerEndpointActionRequest{
		SiteID:      fixture.siteID,
		Action:      model.SiteExplorerEndpointActionClearError,
		Target:      model.SiteExplorerEndpointTargetEndpointIDs,
		EndpointIDs: []string{"10.0.0.1"},
	})
	assert.Equal(t, http.StatusForbidden, rec.Code)
	assert.Empty(t, fixture.proxiedReqs)
}

type siteExplorerEndpointActionHandlerFixture struct {
	org         string
	siteID      string
	user        interface{}
	handler     SiteExplorerEndpointActionHandler
	tsc         *tmocks.Client
	proxiedReqs []grpcproxy.Request
}

func newSiteExplorerEndpointActionHandlerFixture(t *testing.T, roles []string) *siteExplorerEndpointActionHandlerFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	org := "test-org-" + uuid.NewString()
	user := common.TestBuildUser(t, dbSession, "test-starfleet-id-"+uuid.NewString(), org, roles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, user)
	site := common.TestBuildSite(t, dbSession, ip, "Test Site", user)
	sDAO := cdbm.NewSiteDAO(dbSession)
	_, err := sDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)

	tsc := &tmocks.Client{}
	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = tsc

	return &siteExplorerEndpointActionHandlerFixture{
		org:     org,
		siteID:  site.ID.String(),
		user:    user,
		handler: NewSiteExplorerEndpointActionHandler(dbSession, scp, common.GetTestConfig()),
		tsc:     tsc,
	}
}

func (f *siteExplorerEndpointActionHandlerFixture) expectProxyResponse(t *testing.T, resp proto.Message) {
	t.Helper()

	wrun := &tmocks.WorkflowRun{}
	wrun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		if resp == nil {
			return
		}
		out := args.Get(1).(*grpcproxy.Response)
		responseJSON, err := protojson.Marshal(resp)
		require.NoError(t, err)
		out.ResponseJSON = responseJSON
	}).Return(nil).Once()

	f.tsc.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		grpcproxy.Core.WorkflowName,
		mock.Anything,
	).Run(func(args mock.Arguments) {
		f.proxiedReqs = append(f.proxiedReqs, args.Get(3).(grpcproxy.Request))
	}).Return(wrun, nil).Once()
}

func (f *siteExplorerEndpointActionHandlerFixture) request(t *testing.T, apiReq model.APISiteExplorerEndpointActionRequest) *httptest.ResponseRecorder {
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

	require.NoError(t, f.handler.Handle(ec))
	return rec
}
