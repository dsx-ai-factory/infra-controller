// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	enums "go.temporal.io/api/enums/v1"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
)

// templatedProxyFixture wires up a tenant-owned Templated iPXE OS test
// environment with one registered site and a Public template available there.
type templatedProxyFixture struct {
	ctx       context.Context
	dbSession *cdb.Session
	cfg       *config.Config
	scp       *sc.ClientPool
	tc        *tmocks.Client

	ipOrg string
	tnOrg string
	ipu   *cdbm.User
	tnu   *cdbm.User
	ip    *cdbm.InfrastructureProvider
	tn    *cdbm.Tenant
	site  *cdbm.Site
	tmpl  *cdbm.IpxeTemplate
}

func buildTemplatedProxyFixture(t *testing.T) *templatedProxyFixture {
	t.Helper()

	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	common.TestSetupSchema(t, dbSession)

	cfg := common.GetTestConfig()

	ipOrg := "tmpl-proxy-ip-org"
	tnOrg := "tmpl-proxy-tn-org"

	ipu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{authz.ProviderAdminRole})
	tnu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg}, []string{authz.TenantAdminRole})

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg, "tmpl-proxy-provider")
	site := testMachineBuildSite(t, dbSession, ip, "tmpl-proxy-site", cdbm.SiteStatusRegistered)

	tenant := testMachineBuildTenant(t, dbSession, tnOrg, "tmpl-proxy-tenant")
	testBuildTenantSiteAssociation(t, dbSession, tnOrg, tenant.ID, site.ID, tnu.ID)

	templateDAO := cdbm.NewIpxeTemplateDAO(dbSession)
	tmpl, err := templateDAO.Create(ctx, nil, cdbm.IpxeTemplateCreateInput{
		ID:         uuid.New(),
		Name:       "tmpl-proxy-template",
		Template:   "#!ipxe\n",
		Visibility: "Public",
	})
	require.NoError(t, err)
	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(dbSession)
	_, err = itsaDAO.Create(ctx, nil, cdbm.IpxeTemplateSiteAssociationCreateInput{IpxeTemplateID: tmpl.ID, SiteID: site.ID})
	require.NoError(t, err)

	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	t.Cleanup(func() { dbSession.Close() })

	return &templatedProxyFixture{
		ctx:       ctx,
		dbSession: dbSession,
		cfg:       cfg,
		scp:       scp,
		tc:        &tmocks.Client{},
		ipOrg:     ipOrg,
		tnOrg:     tnOrg,
		ipu:       ipu,
		tnu:       tnu,
		ip:        ip,
		tn:        tenant,
		site:      site,
		tmpl:      tmpl,
	}
}

type proxySiteClient struct {
	client   *tmocks.Client
	workflow *tmocks.WorkflowRun
	captured grpcproxy.Request
}

func newProxySiteClient(t *testing.T, wantMethod string, getErr, executeErr error) *proxySiteClient {
	t.Helper()

	psc := &proxySiteClient{
		workflow: &tmocks.WorkflowRun{},
		client:   &tmocks.Client{},
	}
	psc.client.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool {
			if req.FullMethod != wantMethod {
				return false
			}
			psc.captured = req
			return true
		}),
	).Return(psc.workflow, executeErr).Once()
	if executeErr == nil {
		psc.workflow.On("Get", mock.Anything, mock.Anything).Return(getErr).Once()
	}

	return psc
}

func (f *templatedProxyFixture) bindProxyClient(psc *proxySiteClient) {
	f.scp.IDClientMap[f.site.ID.String()] = psc.client
}

func (f *templatedProxyFixture) newEchoContext(method, body string, params map[string]string) (echo.Context, *httptest.ResponseRecorder) {
	return f.newEchoContextForUser(method, body, params, f.tnu)
}

func (f *templatedProxyFixture) newEchoContextForUser(method, body string, params map[string]string, user *cdbm.User) (echo.Context, *httptest.ResponseRecorder) {
	e := echo.New()
	req := httptest.NewRequest(method, "/", strings.NewReader(body))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	names := make([]string, 0, len(params))
	values := make([]string, 0, len(params))
	for k, v := range params {
		names = append(names, k)
		values = append(values, v)
	}
	ec.SetParamNames(names...)
	ec.SetParamValues(values...)
	ec.Set("user", user)
	ec.SetRequest(ec.Request().WithContext(f.ctx))
	return ec, rec
}

func assertProxyCreatePayload(t *testing.T, tnOrg, tmplID string, reqJSON []byte, wantVersion string) {
	t.Helper()
	var coreReq corev1.CreateOperatingSystemRequest
	require.NoError(t, protojson.Unmarshal(reqJSON, &coreReq))
	assert.Equal(t, "tmpl-proxy-os", coreReq.Name)
	assert.Equal(t, tnOrg, coreReq.GetTenantOrganizationId())
	assert.Equal(t, tmplID, coreReq.GetIpxeTemplateId().GetValue())
	require.Len(t, coreReq.IpxeTemplateParameters, 1)
	assert.Equal(t, "version", coreReq.IpxeTemplateParameters[0].Name)
	assert.Equal(t, wantVersion, coreReq.IpxeTemplateParameters[0].Value)
	assert.NotEmpty(t, coreReq.GetId().GetValue())
}

func assertProxyUpdatePayload(t *testing.T, osID, tmplID string, reqJSON []byte, wantVersion string) {
	t.Helper()
	var coreReq corev1.UpdateOperatingSystemRequest
	require.NoError(t, protojson.Unmarshal(reqJSON, &coreReq))
	assert.Equal(t, osID, coreReq.GetId().GetValue())
	assert.Equal(t, tmplID, coreReq.GetIpxeTemplateId().GetValue())
	require.NotNil(t, coreReq.IpxeTemplateParameters)
	require.Len(t, coreReq.IpxeTemplateParameters.Items, 1)
	assert.Equal(t, "version", coreReq.IpxeTemplateParameters.Items[0].Name)
	assert.Equal(t, wantVersion, coreReq.IpxeTemplateParameters.Items[0].Value)
}

func assertProxyDeletePayload(t *testing.T, osID string, reqJSON []byte) {
	t.Helper()
	var coreReq corev1.DeleteOperatingSystemRequest
	require.NoError(t, protojson.Unmarshal(reqJSON, &coreReq))
	assert.Equal(t, osID, coreReq.GetId().GetValue())
}

func (f *templatedProxyFixture) osAssociationStatus(t *testing.T, osID uuid.UUID) string {
	t.Helper()
	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(f.dbSession)
	ossas, _, err := ossaDAO.GetAll(f.ctx, nil,
		cdbm.OperatingSystemSiteAssociationFilterInput{OperatingSystemIDs: []uuid.UUID{osID}},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	require.NoError(t, err)
	require.Len(t, ossas, 1)
	return ossas[0].Status
}

func (f *templatedProxyFixture) osStatus(t *testing.T, osID uuid.UUID) string {
	t.Helper()
	osDAO := cdbm.NewOperatingSystemDAO(f.dbSession)
	os, err := osDAO.GetByID(f.ctx, nil, osID, nil)
	require.NoError(t, err)
	return os.Status
}

// TestOperatingSystemHandler_TemplatedIPXE_Proxy exercises the full create /
// update / delete lifecycle of a Templated iPXE Operating System, which is
// synchronized to its associated Sites through the generic NICo Core gRPC proxy
// (grpcproxy.Core.WorkflowName) rather than the dedicated OsImage workflows used by
// Image based Operating Systems.
func TestOperatingSystemHandler_TemplatedIPXE_Proxy(t *testing.T) {
	f := buildTemplatedProxyFixture(t)

	var osID string

	t.Run("create Templated iPXE syncs to sites via proxy", func(t *testing.T) {
		psc := newProxySiteClient(t, createOperatingSystemMethod, nil, nil)
		f.bindProxyClient(psc)

		createReq := model.APIOperatingSystemCreateRequest{
			Name:           "tmpl-proxy-os",
			Description:    cutil.GetPtr("templated via proxy"),
			IpxeTemplateId: cutil.GetPtr(f.tmpl.ID.String()),
			SiteIDs:        []string{f.site.ID.String()},
			IpxeTemplateParameters: model.APIOperatingSystemIpxeParameters{
				{Name: "version", Value: "22.04"},
			},
		}
		body, merr := json.Marshal(createReq)
		require.NoError(t, merr)

		ec, rec := f.newEchoContext(http.MethodPost, string(body), map[string]string{"orgName": f.tnOrg})
		h := CreateOperatingSystemHandler{dbSession: f.dbSession, tc: f.tc, scp: f.scp, cfg: f.cfg}
		require.NoError(t, h.Handle(ec))
		require.Equal(t, http.StatusCreated, rec.Code, "body: %s", rec.Body.String())

		psc.client.AssertExpectations(t)
		psc.workflow.AssertExpectations(t)
		assert.Equal(t, createOperatingSystemMethod, psc.captured.FullMethod)
		assert.Empty(t, psc.captured.EncryptedSecrets)
		assertProxyCreatePayload(t, f.tnOrg, f.tmpl.ID.String(), psc.captured.RequestJSON, "22.04")

		rsp := &model.APIOperatingSystem{}
		require.NoError(t, json.Unmarshal(rec.Body.Bytes(), rsp))
		require.NotNil(t, rsp.Type)
		assert.Equal(t, cdbm.OperatingSystemTypeTemplatedIPXE, *rsp.Type)
		assert.Equal(t, cdbm.OperatingSystemStatusReady, rsp.Status)
		require.Len(t, rsp.SiteAssociations, 1)
		require.NotNil(t, rsp.SiteAssociations[0].Site)
		assert.Equal(t, f.site.ID.String(), rsp.SiteAssociations[0].Site.ID)
		assert.Equal(t, cdbm.OperatingSystemSiteAssociationStatusSynced, rsp.SiteAssociations[0].Status)

		osID = rsp.ID
		require.NotEmpty(t, osID)
	})

	t.Run("update Templated iPXE re-syncs to sites via proxy", func(t *testing.T) {
		require.NotEmpty(t, osID, "create subtest must run first")

		psc := newProxySiteClient(t, updateOperatingSystemMethod, nil, nil)
		f.bindProxyClient(psc)

		updateReq := model.APIOperatingSystemUpdateRequest{
			Description: cutil.GetPtr("templated via proxy - updated"),
			IpxeTemplateParameters: &model.APIOperatingSystemIpxeParameters{
				{Name: "version", Value: "24.04"},
			},
		}
		body, merr := json.Marshal(updateReq)
		require.NoError(t, merr)

		ec, rec := f.newEchoContext(http.MethodPatch, string(body), map[string]string{"orgName": f.tnOrg, "id": osID})
		h := UpdateOperatingSystemHandler{dbSession: f.dbSession, tc: f.tc, scp: f.scp, cfg: f.cfg}
		require.NoError(t, h.Handle(ec))
		require.Equal(t, http.StatusOK, rec.Code, "body: %s", rec.Body.String())

		psc.client.AssertExpectations(t)
		psc.workflow.AssertExpectations(t)
		assert.Equal(t, updateOperatingSystemMethod, psc.captured.FullMethod)
		assertProxyUpdatePayload(t, osID, f.tmpl.ID.String(), psc.captured.RequestJSON, "24.04")

		rsp := &model.APIOperatingSystem{}
		require.NoError(t, json.Unmarshal(rec.Body.Bytes(), rsp))
		require.NotNil(t, rsp.Description)
		assert.Equal(t, "templated via proxy - updated", *rsp.Description)
		assert.Equal(t, cdbm.OperatingSystemStatusReady, rsp.Status)
		require.Len(t, rsp.SiteAssociations, 1)
		assert.Equal(t, cdbm.OperatingSystemSiteAssociationStatusSynced, rsp.SiteAssociations[0].Status)
	})

	t.Run("delete Templated iPXE pushes delete via proxy and soft-deletes OS", func(t *testing.T) {
		require.NotEmpty(t, osID, "create subtest must run first")

		psc := newProxySiteClient(t, deleteOperatingSystemMethod, nil, nil)
		f.bindProxyClient(psc)

		ec, rec := f.newEchoContext(http.MethodDelete, "", map[string]string{"orgName": f.tnOrg, "id": osID})
		h := DeleteOperatingSystemHandler{dbSession: f.dbSession, tc: f.tc, scp: f.scp, cfg: f.cfg}
		require.NoError(t, h.Handle(ec))
		require.Equal(t, http.StatusAccepted, rec.Code, "body: %s", rec.Body.String())

		psc.client.AssertExpectations(t)
		psc.workflow.AssertExpectations(t)
		assert.Equal(t, deleteOperatingSystemMethod, psc.captured.FullMethod)
		assertProxyDeletePayload(t, osID, psc.captured.RequestJSON)

		parsedID, perr := uuid.Parse(osID)
		require.NoError(t, perr)
		osDAO := cdbm.NewOperatingSystemDAO(f.dbSession)
		_, gerr := osDAO.GetByID(f.ctx, nil, parsedID, nil)
		assert.ErrorIs(t, gerr, cdb.ErrDoesNotExist, "OS should be soft-deleted once every site is cleaned up")
	})
}

func TestOperatingSystemHandler_TemplatedIPXE_ProviderCreateOmitsTenantOrganizationID(t *testing.T) {
	f := buildTemplatedProxyFixture(t)
	psc := newProxySiteClient(t, createOperatingSystemMethod, nil, nil)
	f.bindProxyClient(psc)

	createReq := model.APIOperatingSystemCreateRequest{
		Name:           "tmpl-proxy-provider-os",
		Description:    cutil.GetPtr("provider-owned templated OS"),
		IpxeTemplateId: cutil.GetPtr(f.tmpl.ID.String()),
		SiteIDs:        []string{f.site.ID.String()},
	}
	body, err := json.Marshal(createReq)
	require.NoError(t, err)

	ec, rec := f.newEchoContextForUser(
		http.MethodPost,
		string(body),
		map[string]string{"orgName": f.ipOrg},
		f.ipu,
	)
	h := CreateOperatingSystemHandler{dbSession: f.dbSession, tc: f.tc, scp: f.scp, cfg: f.cfg}
	require.NoError(t, h.Handle(ec))
	require.Equal(t, http.StatusCreated, rec.Code, "body: %s", rec.Body.String())

	psc.client.AssertExpectations(t)
	psc.workflow.AssertExpectations(t)
	var coreReq corev1.CreateOperatingSystemRequest
	require.NoError(t, protojson.Unmarshal(psc.captured.RequestJSON, &coreReq))
	assert.Equal(t, "tmpl-proxy-provider-os", coreReq.Name)
	assert.Nil(t, coreReq.TenantOrganizationId, "provider-owned OS must omit tenant_organization_id")

	var rsp model.APIOperatingSystem
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &rsp))
	osID, err := uuid.Parse(rsp.ID)
	require.NoError(t, err)
	osDAO := cdbm.NewOperatingSystemDAO(f.dbSession)
	persisted, err := osDAO.GetByID(f.ctx, nil, osID, nil)
	require.NoError(t, err)
	require.NotNil(t, persisted.InfrastructureProviderID)
	assert.Equal(t, f.site.InfrastructureProviderID, *persisted.InfrastructureProviderID)
	assert.Nil(t, persisted.TenantID)
}

func TestOperatingSystemHandler_TemplatedIPXE_UpdateRejectsMultipleSites(t *testing.T) {
	f := buildTemplatedProxyFixture(t)

	secondSite := testMachineBuildSite(t, f.dbSession, f.ip, "tmpl-proxy-second-site", cdbm.SiteStatusRegistered)
	testBuildTenantSiteAssociation(t, f.dbSession, f.tnOrg, f.tn.ID, secondSite.ID, f.tnu.ID)
	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(f.dbSession)
	_, err := itsaDAO.Create(f.ctx, nil, cdbm.IpxeTemplateSiteAssociationCreateInput{
		IpxeTemplateID: f.tmpl.ID,
		SiteID:         secondSite.ID,
	})
	require.NoError(t, err)

	originalDescription := "multi-site templated OS"
	osDAO := cdbm.NewOperatingSystemDAO(f.dbSession)
	os, err := osDAO.Create(f.ctx, nil, cdbm.OperatingSystemCreateInput{
		Name:           "tmpl-proxy-multi-site-os",
		Description:    &originalDescription,
		Org:            f.tnOrg,
		TenantID:       &f.tn.ID,
		OsType:         cdbm.OperatingSystemTypeTemplatedIPXE,
		IpxeTemplateId: cutil.GetPtr(f.tmpl.ID.String()),
		Status:         cdbm.OperatingSystemStatusReady,
		CreatedBy:      f.tnu.ID,
	})
	require.NoError(t, err)
	common.TestBuildOperatingSystemSiteAssociation(t, f.dbSession, os.ID, f.site.ID, cutil.GetPtr("site-1"), cdbm.OperatingSystemSiteAssociationStatusSynced, f.tnu)
	common.TestBuildOperatingSystemSiteAssociation(t, f.dbSession, os.ID, secondSite.ID, cutil.GetPtr("site-2"), cdbm.OperatingSystemSiteAssociationStatusSynced, f.tnu)

	updateReq := model.APIOperatingSystemUpdateRequest{
		Description: cutil.GetPtr("must not be persisted"),
	}
	body, err := json.Marshal(updateReq)
	require.NoError(t, err)
	ec, rec := f.newEchoContext(
		http.MethodPatch,
		string(body),
		map[string]string{"orgName": f.tnOrg, "id": os.ID.String()},
	)

	h := UpdateOperatingSystemHandler{dbSession: f.dbSession, tc: f.tc, scp: f.scp, cfg: f.cfg}
	require.NoError(t, h.Handle(ec))
	require.Equal(t, http.StatusBadRequest, rec.Code, "body: %s", rec.Body.String())

	persisted, err := osDAO.GetByID(f.ctx, nil, os.ID, nil)
	require.NoError(t, err)
	require.NotNil(t, persisted.Description)
	assert.Equal(t, originalDescription, *persisted.Description)
}

func TestOperatingSystemHandler_TemplatedIPXE_ProxyCreateExecuteError(t *testing.T) {
	f := buildTemplatedProxyFixture(t)

	psc := newProxySiteClient(t, createOperatingSystemMethod, nil, errors.New("workflow start failed"))
	f.bindProxyClient(psc)

	createReq := model.APIOperatingSystemCreateRequest{
		Name:           "tmpl-proxy-os-error",
		IpxeTemplateId: cutil.GetPtr(f.tmpl.ID.String()),
		SiteIDs:        []string{f.site.ID.String()},
	}
	body, err := json.Marshal(createReq)
	require.NoError(t, err)

	ec, rec := f.newEchoContext(http.MethodPost, string(body), map[string]string{"orgName": f.tnOrg})
	h := CreateOperatingSystemHandler{dbSession: f.dbSession, tc: f.tc, scp: f.scp, cfg: f.cfg}
	require.NoError(t, h.Handle(ec))
	require.Equal(t, http.StatusCreated, rec.Code, "body: %s", rec.Body.String())

	psc.client.AssertExpectations(t)

	rsp := &model.APIOperatingSystem{}
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), rsp))
	assert.Equal(t, cdbm.OperatingSystemStatusError, rsp.Status)
	require.Len(t, rsp.SiteAssociations, 1)
	assert.Equal(t, cdbm.OperatingSystemSiteAssociationStatusError, rsp.SiteAssociations[0].Status)

	parsedID, perr := uuid.Parse(rsp.ID)
	require.NoError(t, perr)
	assert.Equal(t, cdbm.OperatingSystemStatusError, f.osStatus(t, parsedID))
	assert.Equal(t, cdbm.OperatingSystemSiteAssociationStatusError, f.osAssociationStatus(t, parsedID))
}

func TestOperatingSystemHandler_TemplatedIPXE_ProxyCreateTimeout(t *testing.T) {
	f := buildTemplatedProxyFixture(t)

	timeoutErr := tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil)
	psc := newProxySiteClient(t, createOperatingSystemMethod, timeoutErr, nil)
	f.bindProxyClient(psc)

	createReq := model.APIOperatingSystemCreateRequest{
		Name:           "tmpl-proxy-os-timeout",
		IpxeTemplateId: cutil.GetPtr(f.tmpl.ID.String()),
		SiteIDs:        []string{f.site.ID.String()},
	}
	body, err := json.Marshal(createReq)
	require.NoError(t, err)

	ec, rec := f.newEchoContext(http.MethodPost, string(body), map[string]string{"orgName": f.tnOrg})
	h := CreateOperatingSystemHandler{dbSession: f.dbSession, tc: f.tc, scp: f.scp, cfg: f.cfg}
	require.NoError(t, h.Handle(ec))
	require.Equal(t, http.StatusCreated, rec.Code, "body: %s", rec.Body.String())

	psc.client.AssertExpectations(t)
	psc.workflow.AssertExpectations(t)
	assert.Equal(t, createOperatingSystemMethod, psc.captured.FullMethod)

	rsp := &model.APIOperatingSystem{}
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), rsp))
	assert.Equal(t, cdbm.OperatingSystemStatusError, rsp.Status)
	require.Len(t, rsp.SiteAssociations, 1)
	assert.Equal(t, cdbm.OperatingSystemSiteAssociationStatusError, rsp.SiteAssociations[0].Status)

	parsedID, perr := uuid.Parse(rsp.ID)
	require.NoError(t, perr)
	assert.Equal(t, cdbm.OperatingSystemStatusError, f.osStatus(t, parsedID))
	assert.Equal(t, cdbm.OperatingSystemSiteAssociationStatusError, f.osAssociationStatus(t, parsedID))
}

// TestOperatingSystemHandler_TemplatedIPXE_TemplateNotAvailableAtSite verifies that
// creating a Templated iPXE Operating System whose referenced iPXE template has no
// IpxeTemplateSiteAssociation at the target Site is rejected up front (before any
// record is persisted or pushed), since the definition could never render there.
func TestOperatingSystemHandler_TemplatedIPXE_TemplateNotAvailableAtSite(t *testing.T) {
	f := buildTemplatedProxyFixture(t)

	// A template that exists but has no association at the tenant's (only) Site.
	templateDAO := cdbm.NewIpxeTemplateDAO(f.dbSession)
	orphanTmpl, err := templateDAO.Create(f.ctx, nil, cdbm.IpxeTemplateCreateInput{
		ID:         uuid.New(),
		Name:       "tmpl-proxy-template-no-itsa",
		Template:   "#!ipxe\n",
		Visibility: "Public",
	})
	require.NoError(t, err)

	createReq := model.APIOperatingSystemCreateRequest{
		Name:           "tmpl-proxy-os-no-itsa",
		IpxeTemplateId: cutil.GetPtr(orphanTmpl.ID.String()),
		SiteIDs:        []string{f.site.ID.String()},
	}
	body, merr := json.Marshal(createReq)
	require.NoError(t, merr)

	ec, rec := f.newEchoContext(http.MethodPost, string(body), map[string]string{"orgName": f.tnOrg})
	h := CreateOperatingSystemHandler{dbSession: f.dbSession, tc: f.tc, scp: f.scp, cfg: f.cfg}
	require.NoError(t, h.Handle(ec))
	require.Equal(t, http.StatusBadRequest, rec.Code, "body: %s", rec.Body.String())
	assert.Contains(t, rec.Body.String(), "not available at Site")

	// The rejected request must not have persisted an Operating System.
	osDAO := cdbm.NewOperatingSystemDAO(f.dbSession)
	_, tot, gerr := osDAO.GetAll(f.ctx, nil,
		cdbm.OperatingSystemFilterInput{Names: []string{"tmpl-proxy-os-no-itsa"}},
		cdbp.PageInput{Limit: cutil.GetPtr(1)}, nil)
	require.NoError(t, gerr)
	assert.Equal(t, 0, tot)
}
