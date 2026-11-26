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
	"github.com/rs/zerolog"
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
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestGetAllDpuMachinesHandler_Handle(t *testing.T) {
	fixture := newGetAllDpuMachinesHandlerFixture(t, []string{authz.ProviderAdminRole})
	fixture.expectProxyResponse(t, &corev1.MachineIdList{MachineIds: []*corev1.MachineId{
		{Id: "dpu-2"},
		{Id: "dpu-1"},
		{Id: "dpu-3"},
	}})
	fixture.expectProxyResponse(t, &corev1.MachineList{Machines: []*corev1.Machine{
		{
			Id:    &corev1.MachineId{Id: "dpu-2"},
			State: "Ready",
			Status: &corev1.MachineStatus{
				AssociatedHostMachineId: &corev1.MachineId{Id: "host-2"},
			},
		},
		{
			Id:    &corev1.MachineId{Id: "dpu-3"},
			State: "Ready",
			Status: &corev1.MachineStatus{
				AssociatedHostMachineId: &corev1.MachineId{Id: "host-3"},
				LldpNeighbors: []*corev1.InterfaceLldp{
					{
						MacAddress: "02:00:00:00:02:a0",
						Lldp:       &corev1.LldpSwitchData{Name: "leaf-switch-01", LocalPort: "p0"},
					},
				},
			},
		},
	}})

	rec := fixture.request(t, "/?siteId="+fixture.siteID+"&pageNumber=1&pageSize=2&orderBy=ID_DESC")
	require.Equal(t, http.StatusOK, rec.Code)
	require.Len(t, fixture.proxiedReqs, 2)

	var search corev1.MachineSearchConfig
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[0].RequestJSON, &search))
	assert.True(t, search.GetIncludeDpus())
	assert.True(t, search.GetExcludeHosts())
	assert.Equal(t, corev1.Forge_FindMachineIds_FullMethodName, fixture.proxiedReqs[0].FullMethod)

	var byIDs corev1.MachinesByIdsRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[1].RequestJSON, &byIDs))
	assert.Equal(t, []string{"dpu-3", "dpu-2"}, []string{
		byIDs.GetMachineIds()[0].GetId(),
		byIDs.GetMachineIds()[1].GetId(),
	})
	assert.Equal(t, corev1.Forge_FindMachinesByIds_FullMethodName, fixture.proxiedReqs[1].FullMethod)

	var response []model.APIDpuMachine
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
	require.Len(t, response, 2)
	assert.Equal(t, "dpu-3", response[0].ID)
	assert.Equal(t, "host-3", response[0].HostMachineID)
	assert.Nil(t, response[0].DpuNetworkConfig)
	assert.Equal(t, "dpu-2", response[1].ID)
	assert.Equal(t, "host-2", response[1].HostMachineID)
	assert.Equal(t, fixture.siteID, response[0].SiteID)
	assert.Equal(t, fixture.providerID, response[0].InfrastructureProviderID)

	// LLDP neighbors from MachineStatus reach the wire under lldpNeighbors,
	// rendered as [] rather than omitted when nothing was reported.
	var rawResponse []map[string]json.RawMessage
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &rawResponse))
	require.Len(t, response[0].LldpNeighbors, 1)
	assert.Equal(t, "leaf-switch-01", response[0].LldpNeighbors[0].Name)
	assert.Equal(t, "02:00:00:00:02:a0", response[0].LldpNeighbors[0].MacAddress)
	assert.JSONEq(t, `[]`, string(rawResponse[1]["lldpNeighbors"]))

	var pageResponse pagination.PageResponse
	require.NoError(t, json.Unmarshal([]byte(rec.Header().Get(pagination.ResponseHeaderName)), &pageResponse))
	assert.Equal(t, 3, pageResponse.Total)
	assert.Equal(t, dpuMachineOrderByIDDesc, *pageResponse.OrderBy)
}

func TestGetAllDpuMachinesHandler_HandleRejectsInvalidRequests(t *testing.T) {
	tests := []struct {
		name    string
		roles   []string
		target  string
		status  int
		message string
	}{
		{
			name:    "missing site ID",
			roles:   []string{authz.ProviderAdminRole},
			target:  "/",
			status:  http.StatusBadRequest,
			message: "Error validating DPU Machine retrieval request data",
		},
		{name: "unknown query", roles: []string{authz.ProviderAdminRole}, target: "/?siteId=00000000-0000-0000-0000-000000000001&unknown=true", status: http.StatusBadRequest},
		{name: "tenant role", roles: []string{authz.TenantAdminRole}, status: http.StatusForbidden},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := newGetAllDpuMachinesHandlerFixture(t, tt.roles)
			target := tt.target
			if target == "" {
				target = "/?siteId=" + fixture.siteID
			}
			rec := fixture.request(t, target)
			assert.Equal(t, tt.status, rec.Code)
			assert.Empty(t, fixture.proxiedReqs)
			if tt.message != "" {
				var apiErr struct {
					Message string `json:"message"`
				}
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiErr))
				assert.Equal(t, tt.message, apiErr.Message)
			}
		})
	}
}

func TestGetDpuMachineHandler_Handle(t *testing.T) {
	tests := []struct {
		name               string
		roles              []string
		target             func(*getDpuMachineHandlerFixture) string
		expectSearch       bool
		searchMachineIDs   []string
		machine            *corev1.Machine
		networkConfig      *corev1.ManagedHostNetworkConfigResponse
		wantStatus         int
		wantNetworkConfig  bool
		wantProxiedMethods []string
		wantErrorMessage   string
	}{
		{
			name:  "retrieves DPU without network configuration by default",
			roles: []string{authz.ProviderAdminRole},
			target: func(f *getDpuMachineHandlerFixture) string {
				return "/?siteId=" + f.siteID
			},
			machine: &corev1.Machine{
				Id:          &corev1.MachineId{Id: "dpu-1"},
				MachineType: corev1.MachineType_DPU,
				State:       "Ready",
				Status: &corev1.MachineStatus{
					AssociatedHostMachineId: &corev1.MachineId{Id: "host-1"},
				},
			},
			expectSearch:       true,
			searchMachineIDs:   []string{"dpu-1"},
			wantStatus:         http.StatusOK,
			wantProxiedMethods: []string{corev1.Forge_FindMachineIds_FullMethodName, corev1.Forge_FindMachinesByIds_FullMethodName},
		},
		{
			name:  "includes network configuration when requested",
			roles: []string{authz.ProviderAdminRole},
			target: func(f *getDpuMachineHandlerFixture) string {
				return "/?siteId=" + f.siteID + "&includeNetworkConfig=true"
			},
			machine: &corev1.Machine{
				Id:          &corev1.MachineId{Id: "dpu-1"},
				MachineType: corev1.MachineType_DPU,
				State:       "Ready",
				Status: &corev1.MachineStatus{
					AssociatedHostMachineId: &corev1.MachineId{Id: "host-1"},
				},
			},
			expectSearch:      true,
			searchMachineIDs:  []string{"dpu-1"},
			networkConfig:     &corev1.ManagedHostNetworkConfigResponse{Asn: 65001},
			wantStatus:        http.StatusOK,
			wantNetworkConfig: true,
			wantProxiedMethods: []string{
				corev1.Forge_FindMachineIds_FullMethodName,
				corev1.Forge_FindMachinesByIds_FullMethodName,
				corev1.Forge_GetManagedHostNetworkConfig_FullMethodName,
			},
		},
		{
			name:  "rejects a host Machine ID",
			roles: []string{authz.ProviderAdminRole},
			target: func(f *getDpuMachineHandlerFixture) string {
				return "/?siteId=" + f.siteID
			},
			machine: &corev1.Machine{
				Id:          &corev1.MachineId{Id: "dpu-1"},
				MachineType: corev1.MachineType_HOST,
			},
			expectSearch:       true,
			searchMachineIDs:   []string{"dpu-1"},
			wantStatus:         http.StatusNotFound,
			wantProxiedMethods: []string{corev1.Forge_FindMachineIds_FullMethodName, corev1.Forge_FindMachinesByIds_FullMethodName},
			wantErrorMessage:   "Could not find DPU Machine with specified ID",
		},
		{
			name:  "returns not found without forwarding an absent ID",
			roles: []string{authz.ProviderAdminRole},
			target: func(f *getDpuMachineHandlerFixture) string {
				return "/?siteId=" + f.siteID
			},
			expectSearch:       true,
			searchMachineIDs:   []string{"dpu-2"},
			wantStatus:         http.StatusNotFound,
			wantProxiedMethods: []string{corev1.Forge_FindMachineIds_FullMethodName},
			wantErrorMessage:   "Could not find DPU Machine with specified ID",
		},
		{
			name:  "rejects unknown query parameters",
			roles: []string{authz.ProviderAdminRole},
			target: func(f *getDpuMachineHandlerFixture) string {
				return "/?siteId=" + f.siteID + "&unknown=true"
			},
			wantStatus: http.StatusBadRequest,
		},
		{
			name:  "rejects malformed network configuration flag",
			roles: []string{authz.ProviderAdminRole},
			target: func(f *getDpuMachineHandlerFixture) string {
				return "/?siteId=" + f.siteID + "&includeNetworkConfig=invalid"
			},
			wantStatus: http.StatusBadRequest,
		},
		{
			name:  "rejects tenant role",
			roles: []string{authz.TenantAdminRole},
			target: func(f *getDpuMachineHandlerFixture) string {
				return "/?siteId=" + f.siteID
			},
			wantStatus: http.StatusForbidden,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := newGetDpuMachineHandlerFixture(t, tt.roles)
			if tt.expectSearch {
				machineIDs := make([]*corev1.MachineId, 0, len(tt.searchMachineIDs))
				for _, machineID := range tt.searchMachineIDs {
					machineIDs = append(machineIDs, &corev1.MachineId{Id: machineID})
				}
				fixture.expectProxyResponse(t, &corev1.MachineIdList{MachineIds: machineIDs})
			}
			if tt.machine != nil {
				fixture.expectProxyResponse(t, &corev1.MachineList{Machines: []*corev1.Machine{tt.machine}})
			}
			if tt.networkConfig != nil {
				fixture.expectProxyResponse(t, tt.networkConfig)
			}

			rec := fixture.request(t, tt.target(fixture), "dpu-1")
			assert.Equal(t, tt.wantStatus, rec.Code)
			require.Len(t, fixture.proxiedReqs, len(tt.wantProxiedMethods))
			for i, wantMethod := range tt.wantProxiedMethods {
				assert.Equal(t, wantMethod, fixture.proxiedReqs[i].FullMethod)
			}
			if tt.expectSearch {
				var searchRequest corev1.MachineSearchConfig
				require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[0].RequestJSON, &searchRequest))
				assert.True(t, searchRequest.GetIncludeDpus())
				assert.True(t, searchRequest.GetExcludeHosts())
			}
			if tt.wantNetworkConfig {
				var networkConfigRequest corev1.ManagedHostNetworkConfigRequest
				require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[2].RequestJSON, &networkConfigRequest))
				assert.Equal(t, "dpu-1", networkConfigRequest.GetDpuMachineId().GetId())
			}

			if tt.wantStatus == http.StatusOK {
				var response model.APIDpuMachine
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
				assert.Equal(t, "dpu-1", response.ID)
				assert.Equal(t, "host-1", response.HostMachineID)
				assert.Equal(t, tt.wantNetworkConfig, response.DpuNetworkConfig != nil)
			}
			if tt.wantErrorMessage != "" {
				var apiErr struct {
					Message string `json:"message"`
				}
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiErr))
				assert.Equal(t, tt.wantErrorMessage, apiErr.Message)
			}
		})
	}
}

func TestResolveDpuMachine(t *testing.T) {
	dpuMachine := &corev1.Machine{
		Id:          &corev1.MachineId{Id: "dpu-1"},
		MachineType: corev1.MachineType_DPU,
		Status:      &corev1.MachineStatus{AssociatedHostMachineId: &corev1.MachineId{Id: "host-1"}},
	}
	coreRejection := testMachineCoreFailure(status.Error(codes.FailedPrecondition, "Core rejected the Machine lookup"))

	tests := []struct {
		name        string
		setup       func(*testing.T, *dpuMachineCoreFixture)
		siteID      func(*dpuMachineCoreFixture) string
		wantCode    int
		wantMessage string
		// wantCoreMessage is the Core status message expected within the proxy's rendering of it.
		wantCoreMessage    string
		wantProxiedMethods []string
	}{
		{
			name: "returns the DPU Machine with its Site and Site client",
			setup: func(t *testing.T, f *dpuMachineCoreFixture) {
				f.expectDpuMachine(t, dpuMachine)
			},
			wantProxiedMethods: []string{corev1.Forge_FindMachineIds_FullMethodName, corev1.Forge_FindMachinesByIds_FullMethodName},
		},
		{
			name: "returns not found without looking up an ID Core does not list as a DPU",
			setup: func(t *testing.T, f *dpuMachineCoreFixture) {
				f.expectProxyResponse(t, &corev1.MachineIdList{MachineIds: []*corev1.MachineId{{Id: "dpu-2"}}})
			},
			wantCode:           http.StatusNotFound,
			wantMessage:        "Could not find DPU Machine with specified ID",
			wantProxiedMethods: []string{corev1.Forge_FindMachineIds_FullMethodName},
		},
		{
			name: "returns not found for a host Machine",
			setup: func(t *testing.T, f *dpuMachineCoreFixture) {
				f.expectDpuMachine(t, &corev1.Machine{Id: &corev1.MachineId{Id: "dpu-1"}, MachineType: corev1.MachineType_HOST})
			},
			wantCode:           http.StatusNotFound,
			wantMessage:        "Could not find DPU Machine with specified ID",
			wantProxiedMethods: []string{corev1.Forge_FindMachineIds_FullMethodName, corev1.Forge_FindMachinesByIds_FullMethodName},
		},
		{
			name: "surfaces a Core failure listing DPU Machine IDs",
			setup: func(_ *testing.T, f *dpuMachineCoreFixture) {
				f.expectProxyError(coreRejection)
			},
			wantCode:           http.StatusPreconditionFailed,
			wantCoreMessage:    "Core rejected the Machine lookup",
			wantProxiedMethods: []string{corev1.Forge_FindMachineIds_FullMethodName},
		},
		{
			name: "surfaces a Core failure retrieving the DPU Machine",
			setup: func(t *testing.T, f *dpuMachineCoreFixture) {
				f.expectProxyResponse(t, &corev1.MachineIdList{MachineIds: []*corev1.MachineId{{Id: "dpu-1"}}})
				f.expectProxyError(coreRejection)
			},
			wantCode:           http.StatusPreconditionFailed,
			wantCoreMessage:    "Core rejected the Machine lookup",
			wantProxiedMethods: []string{corev1.Forge_FindMachineIds_FullMethodName, corev1.Forge_FindMachinesByIds_FullMethodName},
		},
		{
			name: "rejects a caller without the Provider Admin role",
			setup: func(_ *testing.T, f *dpuMachineCoreFixture) {
				f.setUserRoles(authz.TenantAdminRole)
			},
			wantCode:    http.StatusForbidden,
			wantMessage: "User does not have Provider Admin role with org",
		},
		{
			name:     "rejects a Site that does not exist",
			siteID:   func(*dpuMachineCoreFixture) string { return uuid.NewString() },
			wantCode: http.StatusBadRequest,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := newDpuMachineCoreFixture(t)
			if tt.setup != nil {
				tt.setup(t, fixture)
			}
			siteID := fixture.siteID
			if tt.siteID != nil {
				siteID = tt.siteID(fixture)
			}

			resolved, apiErr := resolveDpuMachine(resolveDpuMachineInput{
				Ctx:          context.Background(),
				Logger:       zerolog.Nop(),
				DBSession:    fixture.dbSession,
				SCP:          fixture.scp,
				Org:          fixture.org,
				User:         fixture.user,
				SiteID:       siteID,
				DpuMachineID: "dpu-1",
			})

			assert.Equal(t, tt.wantProxiedMethods, fixture.proxiedMethods())
			if tt.wantCode != 0 {
				require.NotNil(t, apiErr)
				assert.Nil(t, resolved)
				assert.Equal(t, tt.wantCode, apiErr.Code)
				if tt.wantMessage != "" {
					assert.Equal(t, tt.wantMessage, apiErr.Message)
				}
				if tt.wantCoreMessage != "" {
					assert.Contains(t, apiErr.Message, tt.wantCoreMessage)
					assert.Nil(t, apiErr.Data)
				}
				return
			}

			require.Nil(t, apiErr)
			require.NotNil(t, resolved)
			assert.Equal(t, "dpu-1", resolved.machine.GetId().GetId())
			assert.Equal(t, "host-1", resolved.machine.GetStatus().GetAssociatedHostMachineId().GetId())
			assert.Equal(t, fixture.siteID, resolved.site.ID.String())
			assert.Same(t, fixture.tsc, resolved.stc)

			var search corev1.MachineSearchConfig
			require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[0].RequestJSON, &search))
			assert.True(t, search.GetIncludeDpus())
			assert.True(t, search.GetExcludeHosts())
			var byIDs corev1.MachinesByIdsRequest
			require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[1].RequestJSON, &byIDs))
			require.Len(t, byIDs.GetMachineIds(), 1)
			assert.Equal(t, "dpu-1", byIDs.GetMachineIds()[0].GetId())
		})
	}
}

// dpuMachineCoreFixture is a Registered Site owned by a Provider Admin, a host
// Machine in the REST DB, and a Site client that replays queued Core proxy
// results in order while recording each proxied request.
type dpuMachineCoreFixture struct {
	dbSession     *cdb.Session
	scp           *sc.ClientPool
	org           string
	siteID        string
	hostMachineID string
	user          *cdbm.User
	tsc           *tmocks.Client
	proxiedReqs   []grpcproxy.Request
}

func newDpuMachineCoreFixture(t *testing.T) *dpuMachineCoreFixture {
	t.Helper()
	base := common.NewTestSetupProviderMachineHandlerFixture(t, nil)
	user, ok := base.User.(*cdbm.User)
	require.True(t, ok)

	tsc := &tmocks.Client{}
	base.SiteClientPool.IDClientMap[base.SiteID] = tsc
	return &dpuMachineCoreFixture{
		dbSession:     base.DBSession,
		scp:           base.SiteClientPool,
		org:           base.Org,
		siteID:        base.SiteID,
		hostMachineID: base.MachineID,
		user:          user,
		tsc:           tsc,
	}
}

// setUserRoles replaces the caller with an org member holding only the given roles.
func (f *dpuMachineCoreFixture) setUserRoles(roles ...string) {
	f.user = &cdbm.User{OrgData: cdbm.OrgData{f.org: cdbm.Org{Name: f.org, Roles: roles}}}
}

func (f *dpuMachineCoreFixture) expectProxyResult(run func(mock.Arguments), err error) {
	wrun := &tmocks.WorkflowRun{}
	get := wrun.On("Get", mock.Anything, mock.Anything)
	if run != nil {
		get = get.Run(run)
	}
	get.Return(err).Once()
	f.tsc.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).
		Run(func(args mock.Arguments) {
			f.proxiedReqs = append(f.proxiedReqs, args.Get(3).(grpcproxy.Request))
		}).Return(wrun, nil).Once()
}

// expectProxyResponse queues a successful Core response for the next proxied call.
func (f *dpuMachineCoreFixture) expectProxyResponse(t *testing.T, response proto.Message) {
	t.Helper()
	f.expectProxyResult(func(args mock.Arguments) {
		out := args.Get(1).(*grpcproxy.Response)
		responseJSON, err := protojson.Marshal(response)
		require.NoError(t, err)
		out.ResponseJSON = responseJSON
	}, nil)
}

// expectProxyError queues a Core failure for the next proxied call.
func (f *dpuMachineCoreFixture) expectProxyError(err error) {
	f.expectProxyResult(nil, err)
}

// expectDpuMachine queues the two Core responses resolveDpuMachine reads for machine.
func (f *dpuMachineCoreFixture) expectDpuMachine(t *testing.T, machine *corev1.Machine) {
	t.Helper()
	f.expectProxyResponse(t, &corev1.MachineIdList{MachineIds: []*corev1.MachineId{machine.GetId()}})
	f.expectProxyResponse(t, &corev1.MachineList{Machines: []*corev1.Machine{machine}})
}

func (f *dpuMachineCoreFixture) proxiedMethods() []string {
	var methods []string
	for _, req := range f.proxiedReqs {
		methods = append(methods, req.FullMethod)
	}
	return methods
}

// request sends one request for dpuMachineID through handler. An empty body sends no JSON payload.
func (f *dpuMachineCoreFixture) request(t *testing.T, handler echo.HandlerFunc, method, target, body, dpuMachineID string) *httptest.ResponseRecorder {
	t.Helper()
	e := echo.New()
	req := httptest.NewRequest(method, target, strings.NewReader(body))
	if body != "" {
		req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	}
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName", "id")
	ec.SetParamValues(f.org, dpuMachineID)
	ec.Set("user", f.user)
	require.NoError(t, handler(ec))
	return rec
}

type getAllDpuMachinesHandlerFixture struct {
	org         string
	siteID      string
	providerID  string
	user        interface{}
	handler     GetAllDpuMachinesHandler
	tsc         *tmocks.Client
	proxiedReqs []grpcproxy.Request
}

type getDpuMachineHandlerFixture struct {
	org         string
	siteID      string
	providerID  string
	user        interface{}
	handler     GetDpuMachineHandler
	tsc         *tmocks.Client
	proxiedReqs []grpcproxy.Request
}

func newGetAllDpuMachinesHandlerFixture(t *testing.T, roles []string) *getAllDpuMachinesHandlerFixture {
	t.Helper()
	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	org := "test-org-" + uuid.NewString()
	user := common.TestBuildUser(t, dbSession, "test-starfleet-id-"+uuid.NewString(), org, roles)
	provider := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, user)
	site := common.TestBuildSite(t, dbSession, provider, "Test Site", user)
	sDAO := cdbm.NewSiteDAO(dbSession)
	_, err := sDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)

	tsc := &tmocks.Client{}
	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = tsc
	return &getAllDpuMachinesHandlerFixture{
		org:        org,
		siteID:     site.ID.String(),
		providerID: provider.ID.String(),
		user:       user,
		handler:    NewGetAllDpuMachinesHandler(dbSession, scp),
		tsc:        tsc,
	}
}

func newGetDpuMachineHandlerFixture(t *testing.T, roles []string) *getDpuMachineHandlerFixture {
	t.Helper()
	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	org := "test-org-" + uuid.NewString()
	user := common.TestBuildUser(t, dbSession, "test-starfleet-id-"+uuid.NewString(), org, roles)
	provider := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, user)
	site := common.TestBuildSite(t, dbSession, provider, "Test Site", user)
	sDAO := cdbm.NewSiteDAO(dbSession)
	_, err := sDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)

	tsc := &tmocks.Client{}
	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = tsc
	return &getDpuMachineHandlerFixture{
		org:        org,
		siteID:     site.ID.String(),
		providerID: provider.ID.String(),
		user:       user,
		handler:    NewGetDpuMachineHandler(dbSession, scp),
		tsc:        tsc,
	}
}

func (f *getAllDpuMachinesHandlerFixture) expectProxyResponse(t *testing.T, response proto.Message) {
	t.Helper()
	wrun := &tmocks.WorkflowRun{}
	wrun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		out := args.Get(1).(*grpcproxy.Response)
		responseJSON, err := protojson.Marshal(response)
		require.NoError(t, err)
		out.ResponseJSON = responseJSON
	}).Return(nil).Once()
	f.tsc.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).
		Run(func(args mock.Arguments) {
			f.proxiedReqs = append(f.proxiedReqs, args.Get(3).(grpcproxy.Request))
		}).Return(wrun, nil).Once()
}

func (f *getDpuMachineHandlerFixture) expectProxyResponse(t *testing.T, response proto.Message) {
	t.Helper()
	wrun := &tmocks.WorkflowRun{}
	wrun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		out := args.Get(1).(*grpcproxy.Response)
		responseJSON, err := protojson.Marshal(response)
		require.NoError(t, err)
		out.ResponseJSON = responseJSON
	}).Return(nil).Once()
	f.tsc.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).
		Run(func(args mock.Arguments) {
			f.proxiedReqs = append(f.proxiedReqs, args.Get(3).(grpcproxy.Request))
		}).Return(wrun, nil).Once()
}

func (f *getAllDpuMachinesHandlerFixture) request(t *testing.T, target string) *httptest.ResponseRecorder {
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

func (f *getDpuMachineHandlerFixture) request(t *testing.T, target string, dpuMachineID string) *httptest.ResponseRecorder {
	t.Helper()
	e := echo.New()
	req := httptest.NewRequest(http.MethodGet, target, nil)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName", "id")
	ec.SetParamValues(f.org, dpuMachineID)
	ec.Set("user", f.user)
	require.NoError(t, f.handler.Handle(ec))
	return rec
}
