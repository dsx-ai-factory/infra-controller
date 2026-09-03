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
)

type spectrumXPartitionFixture struct {
	dbSession   *cdb.Session
	org         string
	otherOrg    string
	user        *cdbm.User
	otherUser   *cdbm.User
	nonAdmin    *cdbm.User
	tenant      *cdbm.Tenant
	otherTenant *cdbm.Tenant
	ip          *cdbm.InfrastructureProvider
	site        *cdbm.Site
	noAllocSit  *cdbm.Site
	proxiedReq  *grpcproxy.Request
	coreVNI     *uint32
	coreErr     error
	scp         *sc.ClientPool
}

// newSpectrumXPartitionFixture builds the Tenant, Site, Allocation and mocked Core proxy a
// SpectrumX Partition handler needs. coreVNI stands in for the VNI the Site allocates.
func newSpectrumXPartitionFixture(t *testing.T) *spectrumXPartitionFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	org := "test-tn-org-1"
	otherOrg := "test-tn-org-2"
	nonAdminOrg := "test-tn-org-3"

	ipOrg := "test-ip-org-1"
	ipUser := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, []string{authz.ProviderAdminRole})
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", ipOrg, ipUser)

	user := common.TestBuildUser(t, dbSession, uuid.NewString(), org, []string{authz.TenantAdminRole})
	otherUser := common.TestBuildUser(t, dbSession, uuid.NewString(), otherOrg, []string{authz.TenantAdminRole})
	nonAdmin := common.TestBuildUser(t, dbSession, uuid.NewString(), nonAdminOrg, []string{"NICO_TENANT_NONADMIN"})

	tenant := common.TestBuildTenant(t, dbSession, "Test Tenant", org, user)
	otherTenant := common.TestBuildTenant(t, dbSession, "Other Tenant", otherOrg, otherUser)

	fx := &spectrumXPartitionFixture{
		dbSession:   dbSession,
		org:         org,
		otherOrg:    otherOrg,
		user:        user,
		otherUser:   otherUser,
		nonAdmin:    nonAdmin,
		tenant:      tenant,
		otherTenant: otherTenant,
		proxiedReq:  &grpcproxy.Request{},
	}

	sDAO := cdbm.NewSiteDAO(dbSession)
	registerSite := func(name string) *cdbm.Site {
		site := common.TestBuildSite(t, dbSession, ip, name, ipUser)
		_, err := sDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{
			SiteID: site.ID,
			Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
		})
		require.NoError(t, err)
		return site
	}

	fx.ip = ip
	fx.site = registerSite("Test Site")
	fx.noAllocSit = registerSite("Test Site No Allocation")

	require.NotNil(t, testBuildTenantSiteAssociation(t, dbSession, org, tenant.ID, fx.site.ID, user.ID))
	require.NotNil(t, testBuildTenantSiteAssociation(t, dbSession, org, tenant.ID, fx.noAllocSit.ID, user.ID))
	require.NotNil(t, testBuildTenantSiteAssociation(t, dbSession, otherOrg, otherTenant.ID, fx.site.ID, otherUser.ID))
	require.NotNil(t, testBuildAllocation(t, dbSession, fx.site, tenant, "test-allocation", user))
	require.NotNil(t, testBuildAllocation(t, dbSession, fx.site, otherTenant, "other-allocation", otherUser))

	wrun := &tmocks.WorkflowRun{}
	// The Core response the proxy decodes into the caller's `resp` message is returned
	// through the workflow result, so the mock fills it from the fixture's coreVNI.
	wrun.On("Get", mock.Anything, mock.Anything).Return(func(_ context.Context, out interface{}) error {
		if fx.coreErr != nil {
			return fx.coreErr
		}
		resp, ok := out.(*grpcproxy.Response)
		if !ok || fx.coreVNI == nil {
			return nil
		}
		partition := &corev1.SpxPartition{Vni: *fx.coreVNI}
		payload, err := protojson.Marshal(partition)
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

	fx.scp = sc.NewClientPool(nil)
	fx.scp.IDClientMap[fx.site.ID.String()] = tsc
	fx.scp.IDClientMap[fx.noAllocSit.ID.String()] = tsc

	return fx
}

func (f *spectrumXPartitionFixture) newContext(t *testing.T, method, target string, body string, user *cdbm.User, org string, pathID string) (echo.Context, *httptest.ResponseRecorder) {
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

func (f *spectrumXPartitionFixture) create(t *testing.T, request model.APISpectrumXPartitionCreateRequest, user *cdbm.User, org string) *httptest.ResponseRecorder {
	t.Helper()

	body, err := json.Marshal(request)
	require.NoError(t, err)

	ec, rec := f.newContext(t, http.MethodPost, "/", string(body), user, org, "")
	require.NoError(t, NewCreateSpectrumXPartitionHandler(f.dbSession, f.scp, common.GetTestConfig()).Handle(ec))
	return rec
}

// TestCreateSpectrumXPartitionHandler_Handle covers the create gates and the Core proxy
// contract. The proxied request is inspected directly because it is the only place the
// optional VNI distinction and the method path are observable.
func TestCreateSpectrumXPartitionHandler_Handle(t *testing.T) {
	t.Run("creates the Partition, proxies CreateSpxPartition, and records the Site VNI", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)
		fx.coreVNI = cutil.GetPtr(uint32(10200))

		rec := fx.create(t, model.APISpectrumXPartitionCreateRequest{
			Name:        "east-west-net",
			Description: cutil.GetPtr("east-west"),
			SiteID:      fx.site.ID.String(),
			Labels:      map[string]string{"env": "prod"},
		}, fx.user, fx.org)
		require.Equal(t, http.StatusCreated, rec.Code)

		var resp model.APISpectrumXPartition
		require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
		assert.Equal(t, "east-west-net", resp.Name)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusPending, resp.Status)
		assert.Len(t, resp.StatusHistory, 1)
		require.NotNil(t, resp.VNI, "the Site-allocated VNI must reach the create response")
		assert.Equal(t, 10200, *resp.VNI)

		// The proxy call names the Core method and omits the VNI so the Site allocates it.
		assert.Equal(t, "/forge.Forge/CreateSpxPartition", fx.proxiedReq.FullMethod)
		var coreReq corev1.SpxPartitionCreationRequest
		require.NoError(t, protojson.Unmarshal(fx.proxiedReq.RequestJSON, &coreReq))
		assert.Nil(t, coreReq.Vni)
		assert.Equal(t, fx.org, coreReq.GetTenantOrganizationId())
		assert.Equal(t, resp.ID, coreReq.GetId().GetValue())
		assert.Equal(t, "east-west-net", coreReq.GetMetadata().GetName())

		// The row is persisted with the Site-allocated VNI, not just echoed in the response.
		persisted, err := cdbm.NewSpectrumXPartitionDAO(fx.dbSession).Get(context.Background(), nil, uuid.MustParse(resp.ID), nil)
		require.NoError(t, err)
		require.NotNil(t, persisted.VNI)
		assert.Equal(t, 10200, *persisted.VNI)
	})

	t.Run("a requested VNI is carried to Core", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)

		rec := fx.create(t, model.APISpectrumXPartitionCreateRequest{
			Name:   "east-west-net",
			SiteID: fx.site.ID.String(),
			VNI:    cutil.GetPtr(10500),
		}, fx.user, fx.org)
		require.Equal(t, http.StatusCreated, rec.Code)

		var coreReq corev1.SpxPartitionCreationRequest
		require.NoError(t, protojson.Unmarshal(fx.proxiedReq.RequestJSON, &coreReq))
		require.NotNil(t, coreReq.Vni)
		assert.Equal(t, uint32(10500), *coreReq.Vni)
	})

	// A Core failure has to roll the row back, or the Tenant is left with a Partition
	// that exists nowhere on the Site and blocks the name for good.
	t.Run("a Core failure rolls back the persisted row", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)
		fx.coreErr = assert.AnError

		rec := fx.create(t, model.APISpectrumXPartitionCreateRequest{
			Name:   "east-west-net",
			SiteID: fx.site.ID.String(),
		}, fx.user, fx.org)
		assert.GreaterOrEqual(t, rec.Code, http.StatusBadRequest)

		_, total, err := cdbm.NewSpectrumXPartitionDAO(fx.dbSession).GetAll(context.Background(), nil, cdbm.SpectrumXPartitionFilterInput{
			Names: []string{"east-west-net"},
		}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
		require.NoError(t, err)
		assert.Equal(t, 0, total, "the Partition row must not survive a Core failure")
	})

	t.Run("a duplicate name for the Tenant is a conflict", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)

		request := model.APISpectrumXPartitionCreateRequest{Name: "east-west-net", SiteID: fx.site.ID.String()}
		require.Equal(t, http.StatusCreated, fx.create(t, request, fx.user, fx.org).Code)
		assert.Equal(t, http.StatusConflict, fx.create(t, request, fx.user, fx.org).Code)
	})

	// Two Tenants on one Site are independent namespaces, so the same name has to work.
	t.Run("another Tenant may reuse the name on the same Site", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)

		request := model.APISpectrumXPartitionCreateRequest{Name: "east-west-net", SiteID: fx.site.ID.String()}
		require.Equal(t, http.StatusCreated, fx.create(t, request, fx.user, fx.org).Code)
		assert.Equal(t, http.StatusCreated, fx.create(t, request, fx.otherUser, fx.otherOrg).Code)
	})

	t.Run("rejects a caller without Tenant Admin", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)

		rec := fx.create(t, model.APISpectrumXPartitionCreateRequest{
			Name:   "east-west-net",
			SiteID: fx.site.ID.String(),
		}, fx.nonAdmin, "test-tn-org-3")
		assert.Equal(t, http.StatusForbidden, rec.Code)
	})

	t.Run("rejects a Site the Tenant holds no Allocation on", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)

		rec := fx.create(t, model.APISpectrumXPartitionCreateRequest{
			Name:   "east-west-net",
			SiteID: fx.noAllocSit.ID.String(),
		}, fx.user, fx.org)
		assert.Equal(t, http.StatusForbidden, rec.Code)
	})

	t.Run("rejects an unknown Site", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)

		rec := fx.create(t, model.APISpectrumXPartitionCreateRequest{
			Name:   "east-west-net",
			SiteID: uuid.NewString(),
		}, fx.user, fx.org)
		assert.Equal(t, http.StatusNotFound, rec.Code)
	})

	t.Run("rejects a request that fails validation", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)

		rec := fx.create(t, model.APISpectrumXPartitionCreateRequest{SiteID: fx.site.ID.String()}, fx.user, fx.org)
		assert.Equal(t, http.StatusBadRequest, rec.Code)
	})
}

// TestGetSpectrumXPartitionHandler_Handle covers the read path and the Tenant scoping that
// keeps one Tenant from reading another's Partition by ID.
func TestGetSpectrumXPartitionHandler_Handle(t *testing.T) {
	fx := newSpectrumXPartitionFixture(t)
	handler := NewGetSpectrumXPartitionHandler(fx.dbSession, common.GetTestConfig())

	sxp := testBuildSpectrumXPartition(t, fx.dbSession, "east-west-net", fx.org, fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusReady)

	t.Run("returns the Partition with its expanded relations", func(t *testing.T) {
		ec, rec := fx.newContext(t, http.MethodGet, "/?includeRelation=Site&includeRelation=Tenant", "", fx.user, fx.org, sxp.ID.String())
		require.NoError(t, handler.Handle(ec))
		require.Equal(t, http.StatusOK, rec.Code)

		var resp model.APISpectrumXPartition
		require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
		assert.Equal(t, sxp.ID.String(), resp.ID)
		assert.Equal(t, "east-west-net", resp.Name)
		require.NotNil(t, resp.Site)
		assert.Equal(t, fx.site.ID.String(), resp.Site.ID)
		require.NotNil(t, resp.Tenant)
	})

	t.Run("rejects a Partition owned by another Tenant", func(t *testing.T) {
		ec, rec := fx.newContext(t, http.MethodGet, "/", "", fx.otherUser, fx.otherOrg, sxp.ID.String())
		require.NoError(t, handler.Handle(ec))
		assert.Equal(t, http.StatusBadRequest, rec.Code)
	})

	t.Run("returns 404 for an unknown ID", func(t *testing.T) {
		ec, rec := fx.newContext(t, http.MethodGet, "/", "", fx.user, fx.org, uuid.NewString())
		require.NoError(t, handler.Handle(ec))
		assert.Equal(t, http.StatusNotFound, rec.Code)
	})

	t.Run("returns 400 for a malformed ID", func(t *testing.T) {
		ec, rec := fx.newContext(t, http.MethodGet, "/", "", fx.user, fx.org, "not-a-uuid")
		require.NoError(t, handler.Handle(ec))
		assert.Equal(t, http.StatusBadRequest, rec.Code)
	})
}

// TestGetAllSpectrumXPartitionHandler_Handle covers the list filters and the Tenant scoping
// that the collection endpoint relies on.
func TestGetAllSpectrumXPartitionHandler_Handle(t *testing.T) {
	fx := newSpectrumXPartitionFixture(t)
	handler := NewGetAllSpectrumXPartitionHandler(fx.dbSession, common.GetTestConfig())

	testBuildSpectrumXPartition(t, fx.dbSession, "ready-net", fx.org, fx.site, fx.tenant, cutil.GetPtr(10200), cdbm.SpectrumXPartitionStatusReady)
	testBuildSpectrumXPartition(t, fx.dbSession, "pending-net", fx.org, fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusPending)
	testBuildSpectrumXPartition(t, fx.dbSession, "other-tenant-net", fx.otherOrg, fx.site, fx.otherTenant, nil, cdbm.SpectrumXPartitionStatusReady)

	list := func(t *testing.T, target string, user *cdbm.User, org string) []model.APISpectrumXPartition {
		t.Helper()
		ec, rec := fx.newContext(t, http.MethodGet, target, "", user, org, "")
		require.NoError(t, handler.Handle(ec))
		require.Equal(t, http.StatusOK, rec.Code)

		var resp []model.APISpectrumXPartition
		require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
		assert.NotEmpty(t, rec.Header().Get("X-Pagination"), "the list response must carry the pagination header")
		return resp
	}

	t.Run("returns only the calling Tenant's Partitions", func(t *testing.T) {
		resp := list(t, "/", fx.user, fx.org)
		require.Len(t, resp, 2)
		for _, sxp := range resp {
			assert.NotEqual(t, "other-tenant-net", sxp.Name)
		}
	})

	// The documented default is CREATED_ASC, so an omitted orderBy has to return the
	// Partitions in creation order.
	t.Run("defaults to CREATED_ASC when orderBy is omitted", func(t *testing.T) {
		resp := list(t, "/", fx.user, fx.org)
		require.Len(t, resp, 2)
		assert.Equal(t, "ready-net", resp[0].Name)
		assert.Equal(t, "pending-net", resp[1].Name)
		assert.False(t, resp[1].Created.Before(resp[0].Created))
	})

	t.Run("filters by status", func(t *testing.T) {
		resp := list(t, "/?status=Ready", fx.user, fx.org)
		require.Len(t, resp, 1)
		assert.Equal(t, "ready-net", resp[0].Name)
	})

	t.Run("filters by siteId", func(t *testing.T) {
		assert.Len(t, list(t, "/?siteId="+fx.site.ID.String(), fx.user, fx.org), 2)
	})

	t.Run("rejects an unrecognized status", func(t *testing.T) {
		ec, rec := fx.newContext(t, http.MethodGet, "/?status=Bogus", "", fx.user, fx.org, "")
		require.NoError(t, handler.Handle(ec))
		assert.Equal(t, http.StatusBadRequest, rec.Code)
	})

	t.Run("rejects an unrecognized includeRelation", func(t *testing.T) {
		ec, rec := fx.newContext(t, http.MethodGet, "/?includeRelation=Bogus", "", fx.user, fx.org, "")
		require.NoError(t, handler.Handle(ec))
		assert.Equal(t, http.StatusBadRequest, rec.Code)
	})

	t.Run("rejects an unrecognized orderBy", func(t *testing.T) {
		ec, rec := fx.newContext(t, http.MethodGet, "/?orderBy=BOGUS_ASC", "", fx.user, fx.org, "")
		require.NoError(t, handler.Handle(ec))
		assert.Equal(t, http.StatusBadRequest, rec.Code)
	})
}

// TestDeleteSpectrumXPartitionHandler_Handle covers the delete gates, the attachment
// reference block, and the already-deleted-on-Site absorption.
func TestDeleteSpectrumXPartitionHandler_Handle(t *testing.T) {
	deleteRequest := func(t *testing.T, fx *spectrumXPartitionFixture, id string, user *cdbm.User, org string) *httptest.ResponseRecorder {
		t.Helper()
		ec, rec := fx.newContext(t, http.MethodDelete, "/", "", user, org, id)
		require.NoError(t, NewDeleteSpectrumXPartitionHandler(fx.dbSession, fx.scp, common.GetTestConfig()).Handle(ec))
		return rec
	}

	t.Run("moves the Partition to Deleting and proxies DeleteSpxPartition", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)
		sxp := testBuildSpectrumXPartition(t, fx.dbSession, "east-west-net", fx.org, fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusReady)

		rec := deleteRequest(t, fx, sxp.ID.String(), fx.user, fx.org)
		require.Equal(t, http.StatusAccepted, rec.Code)

		assert.Equal(t, "/forge.Forge/DeleteSpxPartition", fx.proxiedReq.FullMethod)
		var coreReq corev1.SpxPartitionDeletionRequest
		require.NoError(t, protojson.Unmarshal(fx.proxiedReq.RequestJSON, &coreReq))
		assert.Equal(t, sxp.ID.String(), coreReq.GetId().GetValue())

		// The row survives in Deleting; inventory removes it once the Site stops
		// reporting the Partition.
		persisted, err := cdbm.NewSpectrumXPartitionDAO(fx.dbSession).Get(context.Background(), nil, sxp.ID, nil)
		require.NoError(t, err)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusDeleting, persisted.Status)
	})

	// An Instance still attached would lose its network if the Partition went away, so
	// the request is refused rather than cascading.
	t.Run("refuses to delete while an Instance is still attached", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)
		sxp := testBuildSpectrumXPartition(t, fx.dbSession, "east-west-net", fx.org, fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusReady)
		instance := testBuildSpectrumXInstance(t, fx.dbSession, fx.ip, fx.site, fx.tenant, fx.user)

		_, err := cdbm.NewSpectrumXAttachmentDAO(fx.dbSession).Create(context.Background(), nil, cdbm.SpectrumXAttachmentCreateInput{
			InstanceID:           instance.ID,
			SiteID:               fx.site.ID,
			SpectrumXPartitionID: sxp.ID,
			Device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
			DeviceInstance:       0,
			AttachmentType:       cdbm.SpectrumXAttachmentTypePhysical,
			Status:               cdbm.SpectrumXAttachmentStatusReady,
			CreatedBy:            fx.user.ID,
		})
		require.NoError(t, err)

		rec := deleteRequest(t, fx, sxp.ID.String(), fx.user, fx.org)
		assert.Equal(t, http.StatusBadRequest, rec.Code)

		persisted, err := cdbm.NewSpectrumXPartitionDAO(fx.dbSession).Get(context.Background(), nil, sxp.ID, nil)
		require.NoError(t, err)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusReady, persisted.Status, "a refused delete must not change the status")
	})

	t.Run("rejects a Partition owned by another Tenant", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)
		sxp := testBuildSpectrumXPartition(t, fx.dbSession, "east-west-net", fx.org, fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusReady)

		rec := deleteRequest(t, fx, sxp.ID.String(), fx.otherUser, fx.otherOrg)
		assert.Equal(t, http.StatusBadRequest, rec.Code)
	})

	t.Run("returns 404 for an unknown ID", func(t *testing.T) {
		fx := newSpectrumXPartitionFixture(t)
		rec := deleteRequest(t, fx, uuid.NewString(), fx.user, fx.org)
		assert.Equal(t, http.StatusNotFound, rec.Code)
	})
}

// testBuildSpectrumXInstance builds the Instance an attachment row has to reference,
// along with the VPC, Instance Type, Machine and OS it depends on.
func testBuildSpectrumXInstance(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, tenant *cdbm.Tenant, user *cdbm.User) *cdbm.Instance {
	t.Helper()

	instanceType := common.TestBuildInstanceType(t, dbSession, "test-instance-type", nil, site, nil, user)
	machine := common.TestBuildMachine(t, dbSession, ip, site, &instanceType.ID, cutil.GetPtr("mcTypeTest"), cdbm.MachineStatusReady)
	vpc := common.TestBuildVPC(t, dbSession, "test-vpc", ip, tenant, site, nil, nil, nil, cdbm.VpcStatusReady, user)
	os := common.TestBuildOperatingSystem(t, dbSession, "test-os", tenant, cdbm.OperatingSystemStatusReady, user)

	return common.TestBuildInstance(t, dbSession, "test-instance", tenant.ID, ip.ID, site.ID, instanceType.ID, vpc.ID, &machine.ID, os.ID)
}

// testBuildSpectrumXPartition inserts a SpectrumXPartition row directly.
func testBuildSpectrumXPartition(t *testing.T, dbSession *cdb.Session, name, org string, site *cdbm.Site, tenant *cdbm.Tenant, vni *int, status cdbm.SpectrumXPartitionStatus) *cdbm.SpectrumXPartition {
	t.Helper()

	sxp := &cdbm.SpectrumXPartition{
		ID:          uuid.New(),
		Name:        name,
		Description: cutil.GetPtr("Test SpectrumX Partition"),
		Org:         org,
		SiteID:      site.ID,
		TenantID:    tenant.ID,
		VNI:         vni,
		Status:      status,
	}
	_, err := dbSession.DB.NewInsert().Model(sxp).Exec(context.Background())
	require.NoError(t, err)
	return sxp
}
