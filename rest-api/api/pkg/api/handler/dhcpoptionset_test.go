// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalEnums "go.temporal.io/api/enums/v1"
	"go.temporal.io/api/serviceerror"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
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
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

type dhcpOptionSetFixture struct {
	dbSession   *cdb.Session
	org         string
	otherOrg    string
	user        *cdbm.User
	otherUser   *cdbm.User
	nonAdmin    *cdbm.User
	tenant      *cdbm.Tenant
	otherTenant *cdbm.Tenant
	site        *cdbm.Site
	ip          *cdbm.InfrastructureProvider
	ipUser      *cdbm.User
	proxiedReq  *grpcproxy.Request
	coreResp    proto.Message
	coreErr     error
	coreClient  *tmocks.Client
	scp         *sc.ClientPool
}

// newDhcpOptionSetFixture builds the Tenants, Site and mocked Core proxy a DhcpOptionSet handler needs
func newDhcpOptionSetFixture(t *testing.T) *dhcpOptionSetFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	org := "test-tn-org-1"
	otherOrg := "test-tn-org-2"

	ipOrg := "test-ip-org-1"
	ipUser := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, []string{authz.ProviderAdminRole})
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", ipOrg, ipUser)

	user := common.TestBuildUser(t, dbSession, uuid.NewString(), org, []string{authz.TenantAdminRole})
	otherUser := common.TestBuildUser(t, dbSession, uuid.NewString(), otherOrg, []string{authz.TenantAdminRole})
	nonAdmin := common.TestBuildUser(t, dbSession, uuid.NewString(), "test-tn-org-3", []string{"NICO_TENANT_NONADMIN"})

	tenant := common.TestBuildTenant(t, dbSession, "Test Tenant", org, user)
	otherTenant := common.TestBuildTenant(t, dbSession, "Other Tenant", otherOrg, otherUser)

	site := common.TestBuildSite(t, dbSession, ip, "Test Site", ipUser)
	_, err := cdbm.NewSiteDAO(dbSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)

	require.NotNil(t, testBuildTenantSiteAssociation(t, dbSession, org, tenant.ID, site.ID, user.ID))
	require.NotNil(t, testBuildTenantSiteAssociation(t, dbSession, otherOrg, otherTenant.ID, site.ID, otherUser.ID))
	require.NotNil(t, testBuildAllocation(t, dbSession, site, tenant, "test-allocation", user))
	require.NotNil(t, testBuildAllocation(t, dbSession, site, otherTenant, "other-allocation", otherUser))

	fx := &dhcpOptionSetFixture{
		dbSession:   dbSession,
		org:         org,
		otherOrg:    otherOrg,
		user:        user,
		otherUser:   otherUser,
		nonAdmin:    nonAdmin,
		tenant:      tenant,
		otherTenant: otherTenant,
		site:        site,
		ip:          ip,
		ipUser:      ipUser,
		proxiedReq:  &grpcproxy.Request{},
	}

	// The Core response is returned through the workflow result, so the mock fills it from coreResp
	wrun := &tmocks.WorkflowRun{}
	wrun.On("Get", mock.Anything, mock.Anything).Return(func(_ context.Context, out interface{}) error {
		if fx.coreErr != nil {
			return fx.coreErr
		}
		resp, ok := out.(*grpcproxy.Response)
		if !ok || fx.coreResp == nil {
			return nil
		}
		payload, err := protojson.Marshal(fx.coreResp)
		if err != nil {
			return err
		}
		resp.ResponseJSON = payload
		return nil
	})

	tsc := &tmocks.Client{}
	tsc.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool {
			*fx.proxiedReq = req
			return true
		}),
	).Return(wrun, nil)
	fx.coreClient = tsc

	fx.scp = sc.NewClientPool(nil)
	fx.scp.IDClientMap[site.ID.String()] = tsc

	return fx
}

func (f *dhcpOptionSetFixture) newContext(t *testing.T, method, target string, body string, user *cdbm.User, org string, pathID string) (echo.Context, *httptest.ResponseRecorder) {
	t.Helper()

	e := echo.New()
	req := httptest.NewRequest(method, target, strings.NewReader(body))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)

	names := []string{"orgName"}
	values := []string{org}
	if pathID != "" {
		names = append(names, "id")
		values = append(values, pathID)
	}
	ec.SetParamNames(names...)
	ec.SetParamValues(values...)
	ec.Set("user", user)

	return ec, rec
}

// testBuildDhcpOptionSet inserts a Ready DhcpOptionSet row directly
func testBuildDhcpOptionSet(t *testing.T, fx *dhcpOptionSetFixture, name string, tenant *cdbm.Tenant) *cdbm.DhcpOptionSet {
	t.Helper()

	dos, err := cdbm.NewDhcpOptionSetDAO(fx.dbSession).Create(context.Background(), nil, cdbm.DhcpOptionSetCreateInput{
		Name:      name,
		TenantOrg: tenant.Org,
		SiteID:    fx.site.ID,
		TenantID:  tenant.ID,
		Options: &cdbm.DhcpOptions{DhcpOptions: &corev1.DhcpOptions{
			V4: &corev1.DhcpOptionsV4{BootUrl: cutil.GetPtr("http://10.0.0.5/boot.ipxe"), DnsServers: []string{"10.0.0.53"}},
		}},
		Status:    cdbm.DhcpOptionSetStatusReady,
		Version:   cutil.GetPtr("V1-T1"),
		CreatedBy: fx.user.ID,
	})
	require.NoError(t, err)
	return dos
}

func TestCreateDhcpOptionSetHandler_Handle(t *testing.T) {
	const privilegedOrg = "test-tn-org-4"

	tests := []struct {
		name     string
		nonAdmin bool
		caller   func(t *testing.T, fx *dhcpOptionSetFixture) (*cdbm.User, string)
		siteID   string
		coreErr  error
		wantCode int
		check    func(t *testing.T, fx *dhcpOptionSetFixture, rec *httptest.ResponseRecorder)
	}{
		{
			name:     "creates the set, proxies CreateDhcpOptionSet, and records it Ready with the Site version",
			wantCode: http.StatusCreated,
			check: func(t *testing.T, fx *dhcpOptionSetFixture, rec *httptest.ResponseRecorder) {
				var resp model.APIDhcpOptionSet
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
				assert.Equal(t, cdbm.DhcpOptionSetStatusReady, resp.Status)
				assert.Equal(t, []string{"10.0.0.53"}, resp.IPv4.DNSServers)

				assert.Equal(t, "/forge.Forge/CreateDhcpOptionSet", fx.proxiedReq.FullMethod)
				var coreReq corev1.CreateDhcpOptionSetRequest
				require.NoError(t, protojson.Unmarshal(fx.proxiedReq.RequestJSON, &coreReq))
				assert.Equal(t, resp.ID, coreReq.GetId().GetValue())
				assert.Equal(t, fx.org, coreReq.GetTenantOrganizationId())
				assert.Equal(t, "pxe-boot", coreReq.GetMetadata().GetName())
				assert.Equal(t, []string{"10.0.0.53"}, coreReq.GetOptions().GetV4().GetDnsServers())

				persisted, err := cdbm.NewDhcpOptionSetDAO(fx.dbSession).GetByID(context.Background(), nil, uuid.MustParse(resp.ID), nil)
				require.NoError(t, err)
				assert.Equal(t, "V2-T2", persisted.Version)
				assert.Equal(t, cdbm.DhcpOptionSetStatusReady, persisted.Status)
			},
		},
		{
			name:     "a Core failure rolls back the persisted row",
			coreErr:  swe.WrapErr(status.Error(codes.FailedPrecondition, "tenant does not exist on Site")),
			wantCode: http.StatusPreconditionFailed,
			check: func(t *testing.T, fx *dhcpOptionSetFixture, rec *httptest.ResponseRecorder) {
				_, total, err := cdbm.NewDhcpOptionSetDAO(fx.dbSession).GetAll(context.Background(), nil, cdbm.DhcpOptionSetFilterInput{
					Names: []string{"pxe-boot"},
				}, paginator.PageInput{}, nil)
				require.NoError(t, err)
				assert.Equal(t, 0, total, "the row must not survive a Core failure")
			},
		},
		{
			name:     "rejects a caller without Tenant Admin",
			nonAdmin: true,
			wantCode: http.StatusForbidden,
			check: func(t *testing.T, fx *dhcpOptionSetFixture, rec *httptest.ResponseRecorder) {
				fx.coreClient.AssertNumberOfCalls(t, "ExecuteWorkflow", 0)
			},
		},
		{
			name: "provisions the Tenant on the Site for a Tenant with TargetedInstanceCreation and no TenantSite",
			caller: func(t *testing.T, fx *dhcpOptionSetFixture) (*cdbm.User, string) {
				user := common.TestBuildUser(t, fx.dbSession, uuid.NewString(), privilegedOrg, []string{authz.TenantAdminRole})
				tenant := common.TestBuildTenant(t, fx.dbSession, "Privileged Tenant", privilegedOrg, user)
				common.TestBuildTenantAccountWithTargetedInstanceCreation(t, fx.dbSession, fx.ip, &tenant.ID, privilegedOrg, cdbm.TenantAccountStatusReady, user)

				createTenantRun := &tmocks.WorkflowRun{}
				createTenantRun.On("Get", mock.Anything, mock.Anything).Return(nil)
				fx.coreClient.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateTenant", mock.Anything).Return(createTenantRun, nil)
				return user, privilegedOrg
			},
			wantCode: http.StatusCreated,
			check: func(t *testing.T, fx *dhcpOptionSetFixture, rec *httptest.ResponseRecorder) {
				tenantSites, _, err := cdbm.NewTenantSiteDAO(fx.dbSession).GetAll(context.Background(), nil, cdbm.TenantSiteFilterInput{
					TenantOrgs: []string{privilegedOrg},
					SiteIDs:    []uuid.UUID{fx.site.ID},
				}, paginator.PageInput{}, nil)
				require.NoError(t, err)
				assert.Len(t, tenantSites, 1)
				fx.coreClient.AssertCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, "CreateTenant", mock.Anything)
			},
		},
		{
			name:     "rejects a Site that does not exist",
			siteID:   uuid.NewString(),
			wantCode: http.StatusBadRequest,
			check: func(t *testing.T, fx *dhcpOptionSetFixture, rec *httptest.ResponseRecorder) {
				fx.coreClient.AssertNumberOfCalls(t, "ExecuteWorkflow", 0)
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fx := newDhcpOptionSetFixture(t)
			fx.coreResp = &corev1.CreateDhcpOptionSetResponse{DhcpOptionSet: &corev1.DhcpOptionSet{Version: "V2-T2"}}
			fx.coreErr = tt.coreErr

			siteID := fx.site.ID.String()
			if tt.siteID != "" {
				siteID = tt.siteID
			}
			body, err := json.Marshal(model.APIDhcpOptionSetCreateRequest{
				Name:   "pxe-boot",
				SiteID: siteID,
				IPv4:   &model.APIDhcpOptions{DNSServers: []string{"10.0.0.53"}},
			})
			require.NoError(t, err)

			user, org := fx.user, fx.org
			if tt.nonAdmin {
				user, org = fx.nonAdmin, "test-tn-org-3"
			}
			if tt.caller != nil {
				user, org = tt.caller(t, fx)
			}
			ec, rec := fx.newContext(t, http.MethodPost, "/", string(body), user, org, "")
			require.NoError(t, NewCreateDhcpOptionSetHandler(fx.dbSession, fx.scp, common.GetTestConfig()).Handle(ec))
			require.Equal(t, tt.wantCode, rec.Code, rec.Body.String())
			tt.check(t, fx, rec)
		})
	}
}

func TestGetAllDhcpOptionSetHandler_Handle(t *testing.T) {
	fx := newDhcpOptionSetFixture(t)
	testBuildDhcpOptionSet(t, fx, "own-set", fx.tenant)
	testBuildDhcpOptionSet(t, fx, "other-tenant-set", fx.otherTenant)

	// A second Site the Tenant can use holds another set; the Tenant has no access to a third Site.
	site2 := common.TestBuildSite(t, fx.dbSession, fx.ip, "Test Site 2", fx.ipUser)
	require.NotNil(t, testBuildAllocation(t, fx.dbSession, site2, fx.tenant, "test-allocation-2", fx.user))
	_, err := cdbm.NewDhcpOptionSetDAO(fx.dbSession).Create(context.Background(), nil, cdbm.DhcpOptionSetCreateInput{
		Name:      "site2-set",
		TenantOrg: fx.org,
		SiteID:    site2.ID,
		TenantID:  fx.tenant.ID,
		Status:    cdbm.DhcpOptionSetStatusReady,
		CreatedBy: fx.user.ID,
	})
	require.NoError(t, err)
	site3 := common.TestBuildSite(t, fx.dbSession, fx.ip, "Test Site 3", fx.ipUser)

	tests := []struct {
		name      string
		target    string
		wantCode  int
		wantNames []string
	}{
		{name: "returns only the calling Tenant's sets", target: "/", wantCode: http.StatusOK, wantNames: []string{"own-set", "site2-set"}},
		{name: "applies every repeated siteId filter", target: fmt.Sprintf("/?siteId=%s&siteId=%s", fx.site.ID, site2.ID), wantCode: http.StatusOK, wantNames: []string{"own-set", "site2-set"}},
		{name: "validates every repeated status filter", target: "/?status=Ready&status=Bogus", wantCode: http.StatusBadRequest},
		{name: "rejects an unknown query parameter", target: "/?unknown=value", wantCode: http.StatusBadRequest},
		{name: "rejects a siteId the Tenant has no access to", target: "/?siteId=" + site3.ID.String(), wantCode: http.StatusForbidden},
		{name: "rejects an invalid status", target: "/?status=Bogus", wantCode: http.StatusBadRequest},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ec, rec := fx.newContext(t, http.MethodGet, tt.target, "", fx.user, fx.org, "")
			require.NoError(t, NewGetAllDhcpOptionSetHandler(fx.dbSession, common.GetTestConfig()).Handle(ec))
			require.Equal(t, tt.wantCode, rec.Code, rec.Body.String())
			if tt.wantCode != http.StatusOK {
				return
			}

			var resp []model.APIDhcpOptionSet
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
			names := []string{}
			for _, dos := range resp {
				names = append(names, dos.Name)
			}
			assert.ElementsMatch(t, tt.wantNames, names)
			assert.NotEmpty(t, rec.Header().Get("X-Pagination"))
		})
	}
}

func TestGetDhcpOptionSetHandler_Handle(t *testing.T) {
	fx := newDhcpOptionSetFixture(t)
	dos := testBuildDhcpOptionSet(t, fx, "pxe-boot", fx.tenant)

	tests := []struct {
		name     string
		user     *cdbm.User
		org      string
		wantCode int
	}{
		{name: "returns the set", user: fx.user, org: fx.org, wantCode: http.StatusOK},
		{name: "rejects a set owned by another Tenant", user: fx.otherUser, org: fx.otherOrg, wantCode: http.StatusBadRequest},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ec, rec := fx.newContext(t, http.MethodGet, "/", "", tt.user, tt.org, dos.ID.String())
			require.NoError(t, NewGetDhcpOptionSetHandler(fx.dbSession, common.GetTestConfig()).Handle(ec))
			require.Equal(t, tt.wantCode, rec.Code, rec.Body.String())

			if tt.wantCode == http.StatusOK {
				var resp model.APIDhcpOptionSet
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
				assert.Equal(t, dos.ID.String(), resp.ID)
				assert.Equal(t, cutil.GetPtr("http://10.0.0.5/boot.ipxe"), resp.IPv4.BootURL)
			}
		})
	}
}

func TestUpdateDhcpOptionSetHandler_Handle(t *testing.T) {
	tests := []struct {
		name     string
		setup    func(t *testing.T, fx *dhcpOptionSetFixture)
		request  *model.APIDhcpOptionSetUpdateRequest
		coreErr  error
		wantCode int
		check    func(t *testing.T, fx *dhcpOptionSetFixture, persisted *cdbm.DhcpOptionSet)
	}{
		{
			name:     "merges the change, guards with the stored version, and records the new one",
			wantCode: http.StatusOK,
			check: func(t *testing.T, fx *dhcpOptionSetFixture, persisted *cdbm.DhcpOptionSet) {
				assert.Equal(t, "/forge.Forge/UpdateDhcpOptionSet", fx.proxiedReq.FullMethod)
				var coreReq corev1.UpdateDhcpOptionSetRequest
				require.NoError(t, protojson.Unmarshal(fx.proxiedReq.RequestJSON, &coreReq))
				assert.Equal(t, "V1-T1", coreReq.GetIfVersionMatch())
				// The omitted boot URL is kept and the sent DNS servers replace the stored ones
				assert.Equal(t, "http://10.0.0.5/boot.ipxe", coreReq.GetOptions().GetV4().GetBootUrl())
				assert.Equal(t, []string{"10.0.0.54"}, coreReq.GetOptions().GetV4().GetDnsServers())

				assert.Equal(t, "V2-T2", persisted.Version)
				assert.Equal(t, []string{"10.0.0.54"}, persisted.Options.GetV4().GetDnsServers())
			},
		},
		{
			name:     "a Core failure rolls back the update",
			coreErr:  swe.WrapErr(status.Error(codes.FailedPrecondition, "version mismatch")),
			wantCode: http.StatusPreconditionFailed,
			check: func(t *testing.T, fx *dhcpOptionSetFixture, persisted *cdbm.DhcpOptionSet) {
				assert.Equal(t, "V1-T1", persisted.Version)
				assert.Equal(t, []string{"10.0.0.53"}, persisted.Options.GetV4().GetDnsServers())
			},
		},
		{
			name: "rejects a Tenant that lost access to the Site",
			setup: func(t *testing.T, fx *dhcpOptionSetFixture) {
				allocationDAO := cdbm.NewAllocationDAO(fx.dbSession)
				allocations, _, err := allocationDAO.GetAll(context.Background(), nil, cdbm.AllocationFilterInput{
					TenantIDs: []uuid.UUID{fx.tenant.ID},
					SiteIDs:   []uuid.UUID{fx.site.ID},
				}, paginator.PageInput{}, nil)
				require.NoError(t, err)
				for _, allocation := range allocations {
					require.NoError(t, allocationDAO.Delete(context.Background(), nil, allocation.ID))
				}
			},
			wantCode: http.StatusForbidden,
			check: func(t *testing.T, fx *dhcpOptionSetFixture, persisted *cdbm.DhcpOptionSet) {
				fx.coreClient.AssertNumberOfCalls(t, "ExecuteWorkflow", 0)
				assert.Equal(t, "V1-T1", persisted.Version)
			},
		},
		{
			name: "rejects renaming to another set's name on the Site",
			setup: func(t *testing.T, fx *dhcpOptionSetFixture) {
				testBuildDhcpOptionSet(t, fx, "taken-name", fx.tenant)
			},
			request:  &model.APIDhcpOptionSetUpdateRequest{Name: cutil.GetPtr("taken-name")},
			wantCode: http.StatusConflict,
			check: func(t *testing.T, fx *dhcpOptionSetFixture, persisted *cdbm.DhcpOptionSet) {
				fx.coreClient.AssertNumberOfCalls(t, "ExecuteWorkflow", 0)
				assert.Equal(t, "pxe-boot", persisted.Name)
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fx := newDhcpOptionSetFixture(t)
			dos := testBuildDhcpOptionSet(t, fx, "pxe-boot", fx.tenant)
			fx.coreResp = &corev1.UpdateDhcpOptionSetResponse{DhcpOptionSet: &corev1.DhcpOptionSet{Version: "V2-T2"}}
			fx.coreErr = tt.coreErr
			if tt.setup != nil {
				tt.setup(t, fx)
			}

			request := model.APIDhcpOptionSetUpdateRequest{
				IPv4: &model.APIDhcpOptions{DNSServers: []string{"10.0.0.54"}},
			}
			if tt.request != nil {
				request = *tt.request
			}
			body, err := json.Marshal(request)
			require.NoError(t, err)

			ec, rec := fx.newContext(t, http.MethodPatch, "/", string(body), fx.user, fx.org, dos.ID.String())
			require.NoError(t, NewUpdateDhcpOptionSetHandler(fx.dbSession, fx.scp, common.GetTestConfig()).Handle(ec))
			require.Equal(t, tt.wantCode, rec.Code, rec.Body.String())

			persisted, err := cdbm.NewDhcpOptionSetDAO(fx.dbSession).GetByID(context.Background(), nil, dos.ID, nil)
			require.NoError(t, err)
			tt.check(t, fx, persisted)
		})
	}
}

func TestDeleteDhcpOptionSetHandler_Handle(t *testing.T) {
	tests := []struct {
		name     string
		coreErr  error
		wantCode int
	}{
		{name: "deletes the set and proxies DeleteDhcpOptionSet", wantCode: http.StatusNoContent},
		{name: "a set Core no longer has is still deleted", coreErr: swe.WrapErr(status.Error(codes.NotFound, "not found")), wantCode: http.StatusNoContent},
		{name: "a Site refusal keeps the set", coreErr: swe.WrapErr(status.Error(codes.FailedPrecondition, "in use")), wantCode: http.StatusPreconditionFailed},
		{name: "a missing Temporal workflow or namespace keeps the set", coreErr: serviceerror.NewNotFound("workflow not found"), wantCode: http.StatusServiceUnavailable},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fx := newDhcpOptionSetFixture(t)
			dos := testBuildDhcpOptionSet(t, fx, "pxe-boot", fx.tenant)
			fx.coreErr = tt.coreErr

			ec, rec := fx.newContext(t, http.MethodDelete, "/", "", fx.user, fx.org, dos.ID.String())
			require.NoError(t, NewDeleteDhcpOptionSetHandler(fx.dbSession, fx.scp, common.GetTestConfig()).Handle(ec))
			require.Equal(t, tt.wantCode, rec.Code, rec.Body.String())
			if tt.wantCode != http.StatusNoContent {
				_, err := cdbm.NewDhcpOptionSetDAO(fx.dbSession).GetByID(context.Background(), nil, dos.ID, nil)
				assert.NoError(t, err, "the set must be kept when the deletion fails")
				return
			}

			assert.Equal(t, "/forge.Forge/DeleteDhcpOptionSet", fx.proxiedReq.FullMethod)
			fx.coreClient.AssertCalled(t, "ExecuteWorkflow", mock.Anything, mock.MatchedBy(func(options tclient.StartWorkflowOptions) bool {
				return options.ID == "core-grpc-DeleteDhcpOptionSet-"+dos.ID.String() &&
					options.WorkflowIDConflictPolicy == temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING
			}), grpcproxy.Core.WorkflowName, mock.Anything)
			var coreReq corev1.DeleteDhcpOptionSetRequest
			require.NoError(t, protojson.Unmarshal(fx.proxiedReq.RequestJSON, &coreReq))
			assert.Equal(t, dos.ID.String(), coreReq.GetId().GetValue())

			_, err := cdbm.NewDhcpOptionSetDAO(fx.dbSession).GetByID(context.Background(), nil, dos.ID, nil)
			assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
		})
	}
}
