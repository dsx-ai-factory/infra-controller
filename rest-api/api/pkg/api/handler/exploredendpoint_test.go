// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestGetAllExploredEndpointHandler_Handle(t *testing.T) {
	const machineID = "fm100ht4v4mce2qstjnl8970nnj3ie6ecek4mtjn27pea4kre5gsa49jg0g"
	tests := []struct {
		name               string
		roles              []string
		injectSiteID       bool
		extraQuery         string
		rawQuery           string
		ids                *corev1.ExploredEndpointIdList
		proxyError         error
		endpoints          *corev1.ExploredEndpointList
		expectFindByIDs    bool
		expectFindByIDVals []string
		wantStatus         int
		wantCount          int
		wantTotal          int
		wantOrderBy        string
		wantAddresses      []string
		wantProxyCalls     int
		wantMachineID      string
	}{
		{
			name:         "success returns page of endpoints",
			roles:        []string{authz.ProviderAdminRole},
			injectSiteID: true,
			extraQuery:   "",
			ids:          &corev1.ExploredEndpointIdList{EndpointIds: []string{"10.0.0.2", "10.0.0.1"}},
			endpoints: &corev1.ExploredEndpointList{
				Endpoints: []*corev1.ExploredEndpoint{
					{Address: "10.0.0.1", ReportVersion: "1"},
					{Address: "10.0.0.2", ReportVersion: "2"},
				},
			},
			expectFindByIDs:    true,
			expectFindByIDVals: []string{"10.0.0.1", "10.0.0.2"},
			wantStatus:         http.StatusOK,
			wantCount:          2,
			wantTotal:          2,
			wantOrderBy:        exploredEndpointOrderByIDAsc,
			wantAddresses:      []string{"10.0.0.1", "10.0.0.2"},
			wantProxyCalls:     2,
		},
		{
			name:         "success paginates second page",
			roles:        []string{authz.ProviderAdminRole},
			injectSiteID: true,
			extraQuery:   "pageNumber=2&pageSize=1",
			ids:          &corev1.ExploredEndpointIdList{EndpointIds: []string{"10.0.0.1", "10.0.0.2", "10.0.0.3"}},
			endpoints: &corev1.ExploredEndpointList{
				Endpoints: []*corev1.ExploredEndpoint{
					{Address: "10.0.0.2", ReportVersion: "2"},
				},
			},
			expectFindByIDs:    true,
			expectFindByIDVals: []string{"10.0.0.2"},
			wantStatus:         http.StatusOK,
			wantCount:          1,
			wantTotal:          3,
			wantOrderBy:        exploredEndpointOrderByIDAsc,
			wantAddresses:      []string{"10.0.0.2"},
			wantProxyCalls:     2,
		},
		{
			name:           "success empty site skips FindByIds",
			roles:          []string{authz.ProviderAdminRole},
			injectSiteID:   true,
			ids:            &corev1.ExploredEndpointIdList{EndpointIds: []string{}},
			wantStatus:     http.StatusOK,
			wantCount:      0,
			wantTotal:      0,
			wantOrderBy:    exploredEndpointOrderByIDAsc,
			wantProxyCalls: 1,
		},
		{
			name:           "success past-end page skips FindByIds",
			roles:          []string{authz.ProviderAdminRole},
			injectSiteID:   true,
			extraQuery:     "pageNumber=5&pageSize=10",
			ids:            &corev1.ExploredEndpointIdList{EndpointIds: []string{"10.0.0.1"}},
			wantStatus:     http.StatusOK,
			wantCount:      0,
			wantTotal:      1,
			wantOrderBy:    exploredEndpointOrderByIDAsc,
			wantProxyCalls: 1,
		},
		{
			name:         "success orders endpoint IDs descending",
			roles:        []string{authz.ProviderAdminRole},
			injectSiteID: true,
			extraQuery:   "orderBy=ID_DESC&pageSize=2",
			ids:          &corev1.ExploredEndpointIdList{EndpointIds: []string{"10.0.0.2", "10.0.0.1", "10.0.0.3"}},
			endpoints: &corev1.ExploredEndpointList{
				Endpoints: []*corev1.ExploredEndpoint{
					{Address: "10.0.0.2", ReportVersion: "2"},
					{Address: "10.0.0.3", ReportVersion: "3"},
				},
			},
			expectFindByIDs:    true,
			expectFindByIDVals: []string{"10.0.0.3", "10.0.0.2"},
			wantStatus:         http.StatusOK,
			wantCount:          2,
			wantTotal:          3,
			wantOrderBy:        exploredEndpointOrderByIDDesc,
			wantAddresses:      []string{"10.0.0.3", "10.0.0.2"},
			wantProxyCalls:     2,
		},
		{
			name:         "forwards machine filter and paginates returned IDs",
			roles:        []string{authz.ProviderAdminRole},
			injectSiteID: true,
			extraQuery:   "machineId=" + machineID + "&pageNumber=2&pageSize=1&orderBy=ID_DESC",
			ids:          &corev1.ExploredEndpointIdList{EndpointIds: []string{"2001:db8::3", "2001:db8::1", "2001:db8::2"}},
			endpoints: &corev1.ExploredEndpointList{
				Endpoints: []*corev1.ExploredEndpoint{{Address: "2001:db8::2"}},
			},
			expectFindByIDs:    true,
			expectFindByIDVals: []string{"2001:db8::2"},
			wantStatus:         http.StatusOK,
			wantCount:          1,
			wantTotal:          3,
			wantOrderBy:        exploredEndpointOrderByIDDesc,
			wantAddresses:      []string{"2001:db8::2"},
			wantProxyCalls:     2,
			wantMachineID:      machineID,
		},
		{
			name:         "rejects empty machine filter",
			roles:        []string{authz.ProviderAdminRole},
			injectSiteID: true,
			extraQuery:   "machineId=",
			wantStatus:   http.StatusBadRequest,
		},
		{
			name:           "rejects malformed machine filter from Core",
			roles:          []string{authz.ProviderAdminRole},
			injectSiteID:   true,
			extraQuery:     "machineId=not-a-machine-id",
			proxyError:     swe.WrapErr(status.Error(codes.InvalidArgument, "invalid machine ID")),
			wantStatus:     http.StatusBadRequest,
			wantProxyCalls: 1,
		},
		{
			name:         "rejects non provider admin",
			roles:        nil,
			injectSiteID: true,
			extraQuery:   "machineId=" + machineID,
			wantStatus:   http.StatusForbidden,
		},
		{
			name:       "rejects missing siteId",
			roles:      []string{authz.ProviderAdminRole},
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "rejects invalid siteId",
			roles:      []string{authz.ProviderAdminRole},
			rawQuery:   "siteId=not-a-uuid",
			wantStatus: http.StatusBadRequest,
		},
		{
			name:         "rejects invalid pagination",
			roles:        []string{authz.ProviderAdminRole},
			injectSiteID: true,
			extraQuery:   "pageNumber=-1",
			wantStatus:   http.StatusBadRequest,
		},
		{
			name:         "rejects unknown query params",
			roles:        []string{authz.ProviderAdminRole},
			injectSiteID: true,
			extraQuery:   "foo=bar",
			wantStatus:   http.StatusBadRequest,
		},
		{
			name:         "rejects unsupported ordering",
			roles:        []string{authz.ProviderAdminRole},
			injectSiteID: true,
			extraQuery:   "orderBy=NAME_ASC",
			wantStatus:   http.StatusBadRequest,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := newGetAllExploredEndpointHandlerFixture(t, tt.roles)
			query := "/"
			switch {
			case tt.rawQuery != "":
				query = "/?" + tt.rawQuery
			case tt.injectSiteID:
				query = "/?siteId=" + fixture.siteID
				if tt.extraQuery != "" {
					query += "&" + tt.extraQuery
				}
			}

			if tt.ids != nil || tt.proxyError != nil {
				fixture.expectProxyResponse(t, tt.ids, tt.proxyError)
			}
			if tt.expectFindByIDs {
				fixture.expectProxyResponse(t, tt.endpoints, nil)
			}

			rec := fixture.request(t, query)
			assert.Equal(t, tt.wantStatus, rec.Code)
			assert.Len(t, fixture.proxiedReqs, tt.wantProxyCalls)

			if tt.wantStatus != http.StatusOK {
				return
			}
			var filter corev1.ExploredEndpointSearchFilter
			require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[0].RequestJSON, &filter))
			assert.Equal(t, tt.wantMachineID, filter.GetMachineId())
			if tt.wantMachineID == "" {
				assert.Nil(t, filter.MachineId)
			}

			var resp []*model.APIExploredEndpoint
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
			assert.Len(t, resp, tt.wantCount)
			for i, addr := range tt.wantAddresses {
				assert.Equal(t, addr, resp[i].Address)
			}

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			require.NotEmpty(t, ph)
			var pageResp pagination.PageResponse
			require.NoError(t, json.Unmarshal([]byte(ph), &pageResp))
			assert.Equal(t, tt.wantTotal, pageResp.Total)
			assert.Equal(t, tt.wantOrderBy, *pageResp.OrderBy)

			if tt.expectFindByIDs {
				require.Len(t, fixture.proxiedReqs, 2)
				assert.Equal(t, corev1.Forge_FindExploredEndpointIds_FullMethodName, fixture.proxiedReqs[0].FullMethod)
				assert.Equal(t, corev1.Forge_FindExploredEndpointsByIds_FullMethodName, fixture.proxiedReqs[1].FullMethod)
				var byIDs corev1.ExploredEndpointsByIdsRequest
				require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[1].RequestJSON, &byIDs))
				assert.Equal(t, tt.expectFindByIDVals, byIDs.GetEndpointIds())
			} else if tt.wantProxyCalls == 1 {
				assert.Equal(t, corev1.Forge_FindExploredEndpointIds_FullMethodName, fixture.proxiedReqs[0].FullMethod)
			}
		})
	}

	t.Run("success returns every Core ID across multiple pages", func(t *testing.T) {
		fixture := newGetAllExploredEndpointHandlerFixture(t, []string{authz.ProviderAdminRole})
		allIDs := []string{"10.0.0.5", "10.0.0.2", "10.0.0.4", "10.0.0.1", "10.0.0.3"}
		wantPages := [][]string{
			{"10.0.0.1", "10.0.0.2"},
			{"10.0.0.3", "10.0.0.4"},
			{"10.0.0.5"},
		}

		var gotAll []string
		for pageIndex, wantPage := range wantPages {
			fixture.expectProxyResponse(t, &corev1.ExploredEndpointIdList{EndpointIds: allIDs}, nil)
			endpoints := make([]*corev1.ExploredEndpoint, 0, len(wantPage))
			for _, address := range wantPage {
				endpoints = append(endpoints, &corev1.ExploredEndpoint{Address: address})
			}
			fixture.expectProxyResponse(t, &corev1.ExploredEndpointList{Endpoints: endpoints}, nil)

			rec := fixture.request(t, fmt.Sprintf("/?siteId=%s&pageNumber=%d&pageSize=2", fixture.siteID, pageIndex+1))
			require.Equal(t, http.StatusOK, rec.Code)
			require.Len(t, fixture.proxiedReqs, (pageIndex+1)*2)
			var response []*model.APIExploredEndpoint
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
			gotPage := make([]string, 0, len(response))
			for _, endpoint := range response {
				gotPage = append(gotPage, endpoint.Address)
			}
			assert.Equal(t, wantPage, gotPage)
			gotAll = append(gotAll, gotPage...)
			var pageResponse pagination.PageResponse
			require.NoError(t, json.Unmarshal([]byte(rec.Header().Get(pagination.ResponseHeaderName)), &pageResponse))
			assert.Equal(t, len(allIDs), pageResponse.Total)

			idRequestIndex := pageIndex * 2
			byIDRequestIndex := idRequestIndex + 1
			assert.Equal(t, corev1.Forge_FindExploredEndpointIds_FullMethodName, fixture.proxiedReqs[idRequestIndex].FullMethod)
			assert.Equal(t, corev1.Forge_FindExploredEndpointsByIds_FullMethodName, fixture.proxiedReqs[byIDRequestIndex].FullMethod)
			var byIDs corev1.ExploredEndpointsByIdsRequest
			require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[byIDRequestIndex].RequestJSON, &byIDs))
			assert.Equal(t, wantPage, byIDs.GetEndpointIds())
		}

		assert.Equal(t, []string{"10.0.0.1", "10.0.0.2", "10.0.0.3", "10.0.0.4", "10.0.0.5"}, gotAll)
		assert.Len(t, fixture.proxiedReqs, len(wantPages)*2)
	})
}

type getAllExploredEndpointHandlerFixture struct {
	org         string
	siteID      string
	user        interface{}
	handler     GetAllExploredEndpointHandler
	tsc         *tmocks.Client
	proxiedReqs []grpcproxy.Request
}

func newGetAllExploredEndpointHandlerFixture(t *testing.T, roles []string) *getAllExploredEndpointHandlerFixture {
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

	return &getAllExploredEndpointHandlerFixture{
		org:     org,
		siteID:  site.ID.String(),
		user:    user,
		handler: NewGetAllExploredEndpointHandler(dbSession, scp, common.GetTestConfig()),
		tsc:     tsc,
	}
}

func (f *getAllExploredEndpointHandlerFixture) expectProxyResponse(t *testing.T, resp proto.Message, proxyError error) {
	t.Helper()

	wrun := &tmocks.WorkflowRun{}
	wrun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		if proxyError != nil {
			return
		}
		out := args.Get(1).(*grpcproxy.Response)
		responseJSON, err := protojson.Marshal(resp)
		require.NoError(t, err)
		out.ResponseJSON = responseJSON
	}).Return(proxyError).Once()

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

func (f *getAllExploredEndpointHandlerFixture) request(t *testing.T, target string) *httptest.ResponseRecorder {
	t.Helper()

	e := echo.New()
	req := httptest.NewRequest(http.MethodGet, target, nil)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName")
	ec.SetParamValues(f.org)
	ec.Set("user", f.user)

	require.NoError(t, f.handler.Handle(ec))
	return rec
}
