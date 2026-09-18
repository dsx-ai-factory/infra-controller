// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	oteltrace "go.opentelemetry.io/otel/trace"
	"go.temporal.io/api/enums/v1"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"

	"go4.org/netipx"
)

func testVPCPrefixCIDREntity(t *testing.T, vp *cdbm.VpcPrefix) string {
	t.Helper()
	require.NotNil(t, vp)
	p := strings.TrimSpace(vp.Prefix)
	require.NotEmpty(t, p)
	if strings.Contains(p, "/") {
		return p
	}
	return fmt.Sprintf("%s/%d", p, vp.PrefixLength)
}

// testIPv4NthUsableInCIDR returns the nth usable IPv4 address (1-based) inside an IPv4 cidr (/32,/31,/30,… semantics).
func testIPv4NthUsableInCIDR(t *testing.T, cidr string, nth int) string {
	t.Helper()
	pp, err := netip.ParsePrefix(cidr)
	require.NoError(t, err)
	require.True(t, pp.Addr().Is4())
	require.Positive(t, nth)
	bl := int(pp.Bits())

	var usable []netip.Addr
	switch {
	case bl == 31:
		r := netipx.RangeOfPrefix(pp.Masked())
		a0 := r.From()
		a1 := r.From().Next()
		usable = append(usable, a0)
		if pp.Contains(a1) {
			usable = append(usable, a1)
		}
	case bl == 32:
		usable = append(usable, pp.Addr())
	default:
		r := netipx.RangeOfPrefix(pp.Masked())
		first := r.From().Next()
		last := r.To().Prev()
		require.True(t, first.IsValid() && last.IsValid())
		for a := first; a.Compare(last) <= 0; a = a.Next() {
			usable = append(usable, a)
		}
	}

	require.GreaterOrEqual(t, len(usable), nth, "prefix %s has fewer than %d usable addresses", cidr, nth)
	return usable[nth-1].String()
}

func testVpcPrefixBuildDomain(t *testing.T, dbSession *cdb.Session, hostname, org string, userID *uuid.UUID) *cdbm.Domain {
	domain := &cdbm.Domain{
		ID:        uuid.New(),
		Hostname:  hostname,
		Org:       org,
		Status:    cdbm.DomainStatusPending,
		CreatedBy: *userID,
	}
	_, err := dbSession.DB.NewInsert().Model(domain).Exec(context.Background())
	assert.Nil(t, err)
	return domain
}

func testVpcPrefixBuildInterface(t *testing.T, dbSession *cdb.Session, instanceID, subnetID, vpcPrefixID, userID *uuid.UUID) *cdbm.Interface {
	is := &cdbm.Interface{
		ID:          uuid.New(),
		InstanceID:  *instanceID,
		SubnetID:    subnetID,
		VpcPrefixID: vpcPrefixID,
		Status:      cdbm.InterfaceStatusPending,
		CreatedBy:   *userID,
	}
	_, err := dbSession.DB.NewInsert().Model(is).Exec(context.Background())
	assert.Nil(t, err)
	return is
}

func testVpcPrefixBuildVpc(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, tenant *cdbm.Tenant, site *cdbm.Site, org, name string, networkVtType *string, status string, controllerVpcID *uuid.UUID) *cdbm.Vpc {
	vpc := &cdbm.Vpc{
		ID:                        uuid.New(),
		Name:                      name,
		Org:                       org,
		InfrastructureProviderID:  ip.ID,
		TenantID:                  tenant.ID,
		SiteID:                    site.ID,
		NetworkVirtualizationType: networkVtType,
		Status:                    status,
		ControllerVpcID:           controllerVpcID,
		CreatedBy:                 uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(vpc).Exec(context.Background())
	assert.Nil(t, err)
	return vpc
}

func TestVpcPrefixHandler_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2, tnOrg3}, tnRoles)

	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)
	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site2)
	site3 := testIPBlockBuildSite(t, dbSession, ip2, "testSite3", cdbm.SiteStatusPending, true, ipu)
	assert.NotNil(t, site3)
	site4 := testIPBlockBuildSite(t, dbSession, ip2, "testSite4", cdbm.SiteStatusRegistered, false, ipu)
	assert.NotNil(t, site4)

	tenant1 := testMachineBuildTenant(t, dbSession, tnOrg1, "t1")
	tenant2 := testMachineBuildTenant(t, dbSession, tnOrg2, "t2")

	vpc1 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc2 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant1, site2, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc3 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant1, site2, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc4 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant1, site2, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusPending, nil)
	vpc5 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant1, site3, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc6 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant2, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc7 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant2, site4, tnOrg2, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc8 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant2, site4, tnOrg2, "testVPC8", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpcSLAAC := testVpcPrefixBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "testVPCSLAAC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpcDAO := cdbm.NewVpcDAO(dbSession)
	_, err := vpcDAO.Update(ctx, nil, cdbm.VpcUpdateInput{VpcID: vpcSLAAC.ID, SlaacEnabled: cutil.GetPtr(true)})
	require.NoError(t, err)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site.ID.String()] = tsc
	scp.IDClientMap[site2.ID.String()] = tsc
	scp.IDClientMap[site3.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tempClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateVpcPrefix", mock.Anything).Return(wrun, nil)

	// Mock timeout error
	tsc1 := &tmocks.Client{}
	scp.IDClientMap[site4.ID.String()] = tsc1

	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")

	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tsc1.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateVpcPrefix", mock.Anything).Return(wruntimeout, nil)

	tsc1.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref1)

	ipbV6 := testIPBlockBuildIPBlock(t, dbSession, "testipbv6", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "2001:db8:100::", 48, cdbm.IPBlockProtocolVersionV6, false, cdbm.IPBlockStatusReady, ipu)
	parentPrefV6, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbV6.Prefix, ipbV6.PrefixLength, ipbV6.RoutingType, ipbV6.InfrastructureProviderID.String(), ipbV6.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPrefV6)

	ipbPending := testIPBlockBuildIPBlock(t, dbSession, "testipbpending", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.172.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu)
	ipbInvalidProtocol := testIPBlockBuildIPBlock(t, dbSession, "testipbinvalidprotocol", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.173.0.0", 16, "invalid", false, cdbm.IPBlockStatusReady, ipu)

	ipb2 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site2, ip2, &tenant2.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref2, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb2.Prefix, ipb2.PrefixLength, ipb2.RoutingType, ipb2.InfrastructureProviderID.String(), ipb2.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref2)

	ipb3 := testIPBlockBuildIPBlock(t, dbSession, "testipb3", site2, ip2, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.170.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref3, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb3.Prefix, ipb3.PrefixLength, ipb3.RoutingType, ipb3.InfrastructureProviderID.String(), ipb3.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref3)

	ipb4 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site4, ip2, &tenant2.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "168.175.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref4, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb4.Prefix, ipb4.PrefixLength, ipb4.RoutingType, ipb4.InfrastructureProviderID.String(), ipb4.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref4)

	okBodyTimeout, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "oktimeout", VpcID: vpc7.ID.String(), IPBlockID: cutil.GetPtr(ipb4.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)

	ipbFG := testIPBlockBuildIPBlock(t, dbSession, "testipbfg", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.170.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPrefFG, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbFG.Prefix, ipbFG.PrefixLength, ipbFG.RoutingType, ipbFG.InfrastructureProviderID.String(), ipbFG.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPrefFG)
	// This record belongs to the same tenant as the valid block. SitePrefixID is
	// what makes it a SitePrefix rather than an Allocation IPBlock.
	tenantSitePrefix := testIPBlockBuildTenantSitePrefix(t, dbSession, "private-site-prefix", site, ip, tenant1, "192.171.0.0", 16, cdbm.IPBlockStatusReady, ipu)

	okBody, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)

	okBodyFG, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "okFG", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipbFG.ID.String()), PrefixLength: 16})
	assert.Nil(t, err)

	okBodySlash31, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok31", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: 31})
	assert.Nil(t, err)

	errBodySlash32, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "err32", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: 32})
	assert.Nil(t, err)

	okBodyIPv6, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok-v6", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipbV6.ID.String()), PrefixLength: 64})
	assert.Nil(t, err)

	errBodySLAAC64, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "err-slaac-64", VpcID: vpcSLAAC.ID.String(), IPBlockID: cutil.GetPtr(ipbV6.ID.String()), PrefixLength: 64})
	assert.Nil(t, err)

	errBodyPendingIPBlock, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "err-pending-ip-block", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipbPending.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)
	errBodyInvalidIPBlockProtocol, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "err-invalid-ip-block-protocol", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipbInvalidProtocol.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)

	okBodyNameClash, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: vpc2.ID.String(), IPBlockID: cutil.GetPtr(ipb3.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)
	errBodyNameClash, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)

	errBodyDoesntValidate, err := json.Marshal(struct{ Name string }{Name: "test"})
	assert.Nil(t, err)
	errBodyBadVpcID, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: uuid.New().String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)
	errBodyBadVpcTenantMismatch, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: vpc6.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)
	errBodyBadVpcNotFNN, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: vpc8.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)
	errBodyBadIPBlockID, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: 24})
	assert.Nil(t, err)
	errBodyTenantSitePrefixID, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "private-prefix", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(tenantSitePrefix.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)
	errBodyNoIPBlock, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: vpc1.ID.String(), PrefixLength: 25})
	assert.Nil(t, err)

	errBodyBadIPBlockIDTenantMismatch, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb2.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)

	errBodyBadIPBlockIDSiteMismatch, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: vpc3.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)

	errBodyIpamFail, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: 15})
	assert.Nil(t, err)

	errVpcNotReady, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok4", VpcID: vpc4.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)

	errSiteNotReady, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok5", VpcID: vpc5.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		reqBody            string
		user               *cdbm.User
		expectedErr        bool
		expectedStatus     int
		expectedIpam       bool
		expectedErrMsg     string
		expectedPrefix     string
		verifyChildSpanner bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody),
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			reqBody:        string(okBody),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when request body doesnt bind",
			reqOrgName:     tnOrg1,
			reqBody:        "SomeNonJsonBody",
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when request doesnt validate",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyDoesntValidate),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant doesnt exist for org",
			reqOrgName:     tnOrg3,
			reqBody:        string(okBody),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant in request does not match that in org",
			reqOrgName:     tnOrg2,
			reqBody:        string(okBody),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when vpc in request doesnt exist",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyBadVpcID),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when vpc in request doesnt match tenant in org",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyBadVpcTenantMismatch),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when vpc in request is not fnn",
			reqOrgName:     tnOrg2,
			reqBody:        string(errBodyBadVpcNotFNN),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when ipblock is not present in request",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyNoIPBlock),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when ipblock in request doesnt exist",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyBadIPBlockID),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "Tenant SitePrefix cannot be the source IP Block for a VPC Prefix",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyTenantSitePrefixID),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedErrMsg: "Could not find a Ready tenant IP Block specified by ipBlockId",
		},
		{
			name:           "error when IP Block is not Ready",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyPendingIPBlock),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedErrMsg: "Could not find a Ready tenant IP Block specified by ipBlockId",
		},
		{
			name:           "error when IP Block protocol version is unsupported",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyInvalidIPBlockProtocol),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
			expectedErrMsg: "Could not determine VPC Prefix length limit",
		},
		{
			name:           "error when ipblock in request is not derived for tenant",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyBadIPBlockIDTenantMismatch),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when site is mismatched between ipblock and site",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyBadIPBlockIDSiteMismatch),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when ipam creation for vpcprefix fails",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyIpamFail),
			user:           tnu,
			expectedErr:    true,
			expectedErrMsg: "Could not create IPAM entry for VPC prefix. Details: given length:15 must be greater than prefix length:16",
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "success case",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody),
			user:           tnu,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			expectedPrefix: "192.168.0.0/24",
		},
		{
			name:               "success case with Full Grant",
			reqOrgName:         tnOrg1,
			reqBody:            string(okBodyFG),
			user:               tnu,
			expectedErr:        false,
			expectedStatus:     http.StatusCreated,
			expectedPrefix:     "192.170.0.0/16",
			verifyChildSpanner: true,
		},
		{
			name:           "success case with /31",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBodySlash31),
			user:           tnu,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			expectedPrefix: "192.168.1.0/31",
		},
		{
			name:           "success case with IPv6 prefix",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBodyIPv6),
			user:           tnu,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			expectedPrefix: "2001:db8:100::/64",
		},
		{
			name:           "error case with /32",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodySlash32),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when SLAAC IPv6 prefix is /64",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodySLAAC64),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedErrMsg: "prefixLength must be at most 63",
		},
		{
			name:           "error when vpcprefix with same name already exists",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyNameClash),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:           "success when VpcPrefix with same name exists, but for another Site",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBodyNameClash),
			user:           tnu,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			expectedPrefix: "192.170.0.0/24",
		},
		{
			name:           "error when vpc from vpcprefix is not ready state",
			reqOrgName:     tnOrg1,
			reqBody:        string(errVpcNotReady),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when site from vpc is not ready state",
			reqOrgName:     tnOrg1,
			reqBody:        string(errSiteNotReady),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "vpcprefix creation fails, sync workflow timeout",
			reqOrgName:     tnOrg2,
			reqBody:        string(okBodyTimeout),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
	}

	tscCallCount := 0

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(tc.reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.reqOrgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			cipbh := CreateVpcPrefixHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}
			err := cipbh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusCreated)
			require.Equal(t, tc.expectedStatus, rec.Code, rec.Body.String())

			if tc.expectedErr {
				if tc.expectedErrMsg != "" {
					assert.Contains(t, rec.Body.String(), tc.expectedErrMsg)
				}

				return
			}

			resp := &model.APIVpcPrefix{}
			err = json.Unmarshal(rec.Body.Bytes(), resp)
			assert.Nil(t, err)
			// Validate response fields
			assert.Equal(t, len(resp.StatusHistory), 1)
			assert.Equal(t, cdbm.VpcPrefixStatusProvisioning, resp.Status)
			assert.Equal(t, cdbm.VpcPrefixStatusProvisioning, resp.StatusHistory[0].Status)
			// Validate prefix
			assert.NotNil(t, resp.Prefix)
			assert.Equal(t, tc.expectedPrefix, *resp.Prefix)

			// Validate ipam exists for vpcprefix
			if tc.expectedIpam {
				parentIPBID := resp.IPBlockID
				parentIPBUUID, err := uuid.Parse(*parentIPBID)
				assert.Nil(t, err)
				ipbDAO := cdbm.NewIPBlockDAO(dbSession)
				parentIPB, err := ipbDAO.GetByID(ctx, nil, parentIPBUUID, nil)
				assert.Nil(t, err)
				ipamer := cipam.NewWithStorage(ipamStorage)
				ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(ctx, parentIPB.RoutingType, parentIPB.InfrastructureProviderID.String(), parentIPB.SiteID.String()))
				pref := ipamer.PrefixFrom(ctx, ipam.GetCidrForIPBlock(ctx, *resp.Prefix, resp.PrefixLength))
				assert.NotNil(t, pref)
			}

			require.Equal(t, tscCallCount+1, len(tsc.Calls))
			tscCallCount++
			require.Equal(t, "ExecuteWorkflow", tsc.Calls[len(tsc.Calls)-1].Method)
			require.Equal(t, 4, len(tsc.Calls[len(tsc.Calls)-1].Arguments))

			// Check Temporal workflow call arguments
			temporalReq, ok := tsc.Calls[len(tsc.Calls)-1].Arguments[3].(*corev1.VpcPrefixCreationRequest)
			require.True(t, ok)
			assert.Equal(t, resp.Name, temporalReq.Metadata.Name)
			assert.Equal(t, *resp.Prefix, temporalReq.Config.Prefix)

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func testCreateVpcPrefix(t *testing.T, dbSession *cdb.Session, scp *sc.ClientPool, ipamStorage cipam.Storage, user *cdbm.User, reqOrgName, reqBody string) *model.APIVpcPrefix {
	ctx := context.Background()
	e := echo.New()
	req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(reqBody))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName")
	ec.SetParamValues(reqOrgName)
	if user != nil {
		ec.Set("user", user)
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
	ec.SetRequest(ec.Request().WithContext(ctx))

	tc := &tmocks.Client{}

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	for _, tsc := range scp.IDClientMap {
		tsc.(*tmocks.Client).Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
			"CreateVpcPrefix", mock.Anything).Return(wrun, nil)

		// They should all be the same client, so break after the first one.
		break
	}

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	cipbh := CreateVpcPrefixHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       common.GetTestConfig(),
		scp:       scp,
	}
	err := cipbh.Handle(ec)
	assert.Nil(t, err)
	assert.Equal(t, http.StatusCreated, rec.Code)
	rsp := &model.APIVpcPrefix{}
	err = json.Unmarshal(rec.Body.Bytes(), rsp)
	assert.Nil(t, err)
	return rsp
}

func TestVpcPrefixHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2, tnOrg3}, tnRoles)

	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)
	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site2)

	tenant1 := testMachineBuildTenant(t, dbSession, tnOrg1, "t1")
	tenant2 := testMachineBuildTenant(t, dbSession, tnOrg2, "t2")

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tenant1.ID, site.ID, tnu.ID)
	assert.NotNil(t, ts1)

	vpc1 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc2 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant2, site2, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	assert.NotNil(t, vpc2)
	vpc3 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant1, site2, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	assert.NotNil(t, vpc3)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site.ID.String()] = tsc
	scp.IDClientMap[site2.ID.String()] = tsc

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref1)

	ipb2 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site2, ip2, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref2, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb2.Prefix, ipb2.PrefixLength, ipb2.RoutingType, ipb2.InfrastructureProviderID.String(), ipb2.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref2)

	totalCount := 30

	vpDAO := cdbm.NewVpcPrefixDAO(dbSession)
	ss := []cdbm.VpcPrefix{}

	for i := 0; i < totalCount; i++ {
		var vpcID, ipbID uuid.UUID
		if i%2 == 0 {
			vpcID = vpc1.ID
			ipbID = ipb1.ID
		} else {
			vpcID = vpc3.ID
			ipbID = ipb2.ID
		}
		prefixLen := 24
		vpcprefixBody, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: fmt.Sprintf("vpcprefix-%02d", i), VpcID: vpcID.String(), IPBlockID: cutil.GetPtr(ipbID.String()), PrefixLength: prefixLen})
		assert.Nil(t, err)
		apiVpcPrefix := testCreateVpcPrefix(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(vpcprefixBody))
		assert.NotNil(t, apiVpcPrefix)

		sid, _ := uuid.Parse(apiVpcPrefix.ID)
		s, err := vpDAO.GetByID(ctx, nil, sid, nil)
		assert.Nil(t, err)
		common.TestBuildStatusDetail(t, dbSession, sid.String(), cdbm.VpcPrefixStatusProvisioning, cutil.GetPtr("VPC Prefix is being provisioned on Site"))
		ss = append(ss, *s)
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                   string
		reqOrgName             string
		user                   *cdbm.User
		queryVpcID             *string
		querySiteID            *string
		querySearch            *string
		pageNumber             *int
		pageSize               *int
		orderBy                *string
		expectedErr            bool
		expectedStatus         int
		expectedCnt            int
		expectedTotal          *int
		expectedFirstEntry     *cdbm.VpcPrefix
		queryIncludeRelations1 *string
		queryIncludeRelations2 *string
		queryIncludeRelations3 *string
		queryIncludeRelations4 *string
		expectedSiteName       *string
		expectedVpcName        *string
		expectetIPv4Name       *string
		expectedIPv6Name       *string
		verifyChildSpanner     bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when specified org does not have tenant",
			reqOrgName:     tnOrg3,
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when siteId not valid uuid",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySiteID:    cutil.GetPtr("non-uuid"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when siteId in query does not exist",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySiteID:    cutil.GetPtr(uuid.New().String()),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant doesn't have access to siteId in query",
			reqOrgName:     tnOrg2,
			user:           tnu,
			querySiteID:    cutil.GetPtr(site2.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			expectedCnt:    0,
			expectedTotal:  cutil.GetPtr(0),
		},
		{
			name:           "error when vpcId not valid uuid",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryVpcID:     cutil.GetPtr("non-uuid"),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when vpcId in query does not exist",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryVpcID:     cutil.GetPtr(uuid.New().String()),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when vpc's tenant does not match org's tenant",
			reqOrgName:     tnOrg2,
			user:           tnu,
			queryVpcID:     cutil.GetPtr(vpc1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "success when site id is specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySiteID:    cutil.GetPtr(site.ID.String()),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    totalCount / 2,
			expectedTotal:  cutil.GetPtr(totalCount / 2),
		},
		{
			name:           "success when vpc id is specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryVpcID:     cutil.GetPtr(vpc1.ID.String()),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    totalCount / 2,
			expectedTotal:  cutil.GetPtr(totalCount / 2),
		},
		{
			name:               "success when vpc id is not specified",
			reqOrgName:         tnOrg1,
			user:               tnu,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        paginator.DefaultLimit,
			expectedTotal:      &totalCount,
			verifyChildSpanner: true,
		},
		{
			name:               "success when pagination params are specified",
			reqOrgName:         tnOrg1,
			user:               tnu,
			pageNumber:         cutil.GetPtr(1),
			pageSize:           cutil.GetPtr(10),
			orderBy:            cutil.GetPtr("NAME_DESC"),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        10,
			expectedTotal:      &totalCount,
			expectedFirstEntry: &ss[29],
		},
		{
			name:           "failure when invalid pagination params are specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			pageNumber:     cutil.GetPtr(1),
			pageSize:       cutil.GetPtr(10),
			orderBy:        cutil.GetPtr("TEST_ASC"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
		},
		{
			name:                   "success when include Site relation specified",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedCnt:            paginator.DefaultLimit,
			expectedTotal:          &totalCount,
			queryIncludeRelations4: cutil.GetPtr(cdbm.SiteRelationName),
			expectedSiteName:       &site.Name,
		},
		{
			name:                   "success when include Vpc relation specified",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedCnt:            paginator.DefaultLimit,
			expectedTotal:          &totalCount,
			queryIncludeRelations2: cutil.GetPtr(cdbm.VpcRelationName),
			expectedVpcName:        &vpc1.Name,
		},
		{
			name:                   "success when include IP Block relation specified",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedCnt:            paginator.DefaultLimit,
			expectedTotal:          &totalCount,
			queryIncludeRelations1: cutil.GetPtr(cdbm.IPBlockRelationName),
			expectetIPv4Name:       &ipb1.Name,
		},
		{
			name:           "success when query search unexisted name specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr("test"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    0,
			expectedTotal:  cutil.GetPtr(0),
		},
		{
			name:           "success when query search name specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr("vpcprefix"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when query search status specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr("provisioning"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.querySiteID != nil {
				q.Add("siteId", *tc.querySiteID)
			}
			if tc.queryVpcID != nil {
				q.Add("vpcId", *tc.queryVpcID)
			}
			if tc.querySearch != nil {
				q.Add("query", *tc.querySearch)
			}
			if tc.pageNumber != nil {
				q.Set("pageNumber", fmt.Sprintf("%v", *tc.pageNumber))
			}
			if tc.pageSize != nil {
				q.Set("pageSize", fmt.Sprintf("%v", *tc.pageSize))
			}
			if tc.orderBy != nil {
				q.Set("orderBy", *tc.orderBy)
			}
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}
			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations2)
			}
			if tc.queryIncludeRelations3 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations3)
			}
			if tc.queryIncludeRelations4 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations4)
			}

			path := fmt.Sprintf("/v2/org/%s/nico/vpcprefix?%s", tc.reqOrgName, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.reqOrgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			gash := GetAllVpcPrefixHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := gash.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)

			if rec.Code != tc.expectedStatus {
				t.Errorf("response %v", rec.Body.String())
			}
			require.Equal(t, tc.expectedStatus, rec.Code)
			if tc.expectedErr {
				return
			}

			resp := []model.APIVpcPrefix{}
			err = json.Unmarshal(rec.Body.Bytes(), &resp)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedCnt, len(resp))

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			assert.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err = json.Unmarshal([]byte(ph), pr)
			assert.NoError(t, err)

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, pr.Total)
			}

			if tc.expectedFirstEntry != nil {
				assert.Equal(t, tc.expectedFirstEntry.ID.String(), resp[0].ID)
			}

			if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil || tc.queryIncludeRelations3 != nil || tc.queryIncludeRelations4 != nil {
				if tc.expectedVpcName != nil {
					assert.Equal(t, *tc.expectedVpcName, resp[0].Vpc.Name)
				}
				if tc.expectetIPv4Name != nil {
					assert.Equal(t, *tc.expectetIPv4Name, resp[0].IPBlock.Name)
				}
				if tc.expectedSiteName != nil {
					assert.Equal(t, *tc.expectedSiteName, resp[0].Site.Name)
				}
			} else {
				if len(resp) > 0 {
					assert.Nil(t, resp[0].Vpc)
					assert.Nil(t, resp[0].IPBlock)
				}
			}

			for _, apisn := range resp {
				assert.Equal(t, 2, len(apisn.StatusHistory))
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestVpcPrefixHandler_Get(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2, tnOrg3}, tnRoles)

	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)
	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site2)
	tenant1 := testMachineBuildTenant(t, dbSession, tnOrg1, "t1")
	tenant2 := testMachineBuildTenant(t, dbSession, tnOrg2, "t2")
	assert.NotNil(t, tenant2)
	vpc1 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))

	_ = common.TestBuildTenantSite(t, dbSession, tenant1, site, tnu)
	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site.ID.String()] = tsc
	scp.IDClientMap[site2.ID.String()] = tsc

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref1)
	prefixLen := 24
	parentIpbBody, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "ok1", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	vpcprefix := testCreateVpcPrefix(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(parentIpbBody))

	ifaceWorkloadBody, err := json.Marshal(model.APIVpcPrefixCreateRequest{
		Name: "iface-usage-stats", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	require.NoError(t, err)
	vpcprefixWithIfaceWorkload := testCreateVpcPrefix(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(ifaceWorkloadBody))

	ipbV6 := testIPBlockBuildIPBlock(t, dbSession, "testipbv6", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "2001:db8:200::", 48, cdbm.IPBlockProtocolVersionV6, false, cdbm.IPBlockStatusReady, ipu)
	parentPrefV6, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbV6.Prefix, ipbV6.PrefixLength, ipbV6.RoutingType, ipbV6.InfrastructureProviderID.String(), ipbV6.SiteID.String())
	require.NoError(t, err)
	require.NotNil(t, parentPrefV6)
	ipv6Body, err := json.Marshal(model.APIVpcPrefixCreateRequest{
		Name: "ipv6-usage-stats", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipbV6.ID.String()), PrefixLength: 64})
	require.NoError(t, err)
	vpcprefixIPv6 := testCreateVpcPrefix(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(ipv6Body))

	alWorkload := common.TestBuildAllocation(t, dbSession, site, tenant1, "get-vpfx-usage-iface-alloc", ipu)
	itWorkload := common.TestBuildInstanceType(t, dbSession, "get-vpfx-iface-it", cutil.GetPtr(uuid.New()), site, nil, ipu)
	common.TestBuildAllocationConstraint(t, dbSession, alWorkload, itWorkload, nil, 5, ipu)
	mWorkload := common.TestBuildMachine(t, dbSession, ip, site, &itWorkload.ID, cutil.GetPtr("get-vpfx-iface-mt"), cdbm.MachineStatusReady)
	_ = common.TestBuildMachineInstanceType(t, dbSession, mWorkload, itWorkload)
	osWorkload := common.TestBuildOperatingSystem(t, dbSession, "get-vpfx-iface-os", tenant1, cdbm.OperatingSystemStatusReady, tnu)

	vpWorkloadID := uuid.MustParse(vpcprefixWithIfaceWorkload.ID)
	vpWorkloadEnt, err := cdbm.NewVpcPrefixDAO(dbSession).GetByID(ctx, nil, vpWorkloadID, nil)
	require.NoError(t, err)

	cidr := testVPCPrefixCIDREntity(t, vpWorkloadEnt)
	chosenIfaceIPv4 := testIPv4NthUsableInCIDR(t, cidr, 20)

	instWorkload := common.TestBuildInstance(t, dbSession, "get-vpfx-iface-inst", tenant1.ID, ip.ID, site.ID, itWorkload.ID, vpc1.ID, cutil.GetPtr(mWorkload.ID), osWorkload.ID)

	ifWorkload := common.TestBuildInterface(t, dbSession, instWorkload.ID, nil, &vpWorkloadID, true, nil, nil, nil, cutil.GetPtr(cdbm.InterfaceStatusReady), tnu)
	_, err = cdbm.NewInterfaceDAO(dbSession).Update(ctx, nil, cdbm.InterfaceUpdateInput{
		InterfaceID: ifWorkload.ID,
		IpAddresses: []string{chosenIfaceIPv4},
	})
	require.NoError(t, err)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                            string
		reqOrgName                      string
		user                            *cdbm.User
		id                              string
		expectedErr                     bool
		expectedStatus                  int
		queryIncludeRelations1          *string
		queryIncludeRelations2          *string
		queryIncludeRelations3          *string
		queryIncludeUsageStats          *string
		expectedVpcName                 *string
		expectetIPName                  *string
		expectUsageStatsNonNil          bool
		expectUsageStatsNil             bool
		verifyUsageAcquisitionFromIface bool
		verifyChildSpanner              bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			user:           nil,
			id:             vpcprefix.ID,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           tnu,
			id:             vpcprefix.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when vpcprefix id is invalid uuid",
			reqOrgName:     tnOrg1,
			user:           tnu,
			id:             "bad#uuid$str",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified org does not have tenant",
			reqOrgName:     tnOrg3,
			user:           tnu,
			id:             vpcprefix.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when vpcprefix tenant does not match org",
			reqOrgName:     tnOrg2,
			user:           tnu,
			id:             vpcprefix.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified vpcprefix doesnt exist",
			reqOrgName:     tnOrg1,
			user:           tnu,
			id:             uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "success case",
			reqOrgName:     tnOrg1,
			user:           tnu,
			id:             vpcprefix.ID,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
		},
		{
			name:                   "success case when include VPC/Tenant relation",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			id:                     vpcprefix.ID,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			queryIncludeRelations1: cutil.GetPtr(cdbm.VpcRelationName),
			expectedVpcName:        &vpc1.Name,
			verifyChildSpanner:     true,
		},
		{
			name:                   "success case when include IP Block relation",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			id:                     vpcprefix.ID,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			queryIncludeRelations1: cutil.GetPtr(cdbm.IPBlockRelationName),
			expectetIPName:         &ipb1.Name,
		},
		{
			name:                   "error when includeUsageStats query invalid",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			id:                     vpcprefix.ID,
			expectedErr:            true,
			expectedStatus:         http.StatusBadRequest,
			queryIncludeUsageStats: cutil.GetPtr("not-a-bool"),
		},
		{
			name:                   "success case when includeUsageStats true",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			id:                     vpcprefix.ID,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			queryIncludeUsageStats: cutil.GetPtr("true"),
			expectUsageStatsNonNil: true,
		},
		{
			name:                            "success case when includeUsageStats true with ethernet iface and IPv4 in prefix",
			reqOrgName:                      tnOrg1,
			user:                            tnu,
			id:                              vpcprefixWithIfaceWorkload.ID,
			expectedErr:                     false,
			expectedStatus:                  http.StatusOK,
			queryIncludeUsageStats:          cutil.GetPtr("true"),
			expectUsageStatsNonNil:          true,
			verifyUsageAcquisitionFromIface: true,
		},
		{
			name:                   "success case when includeUsageStats true for IPv6 prefix",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			id:                     vpcprefixIPv6.ID,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			queryIncludeUsageStats: cutil.GetPtr("true"),
			expectUsageStatsNil:    true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.queryIncludeUsageStats != nil {
				q.Set("includeUsageStats", *tc.queryIncludeUsageStats)
			}
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}
			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations2)
			}
			if tc.queryIncludeRelations3 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations3)
			}

			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tc.reqOrgName, tc.id)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := GetVpcPrefixHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APIVpcPrefix{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, 1, len(rsp.StatusHistory))

				expanded := tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil || tc.queryIncludeRelations3 != nil || tc.expectUsageStatsNonNil || tc.expectUsageStatsNil
				if expanded {
					if tc.expectedVpcName != nil {
						assert.Equal(t, *tc.expectedVpcName, rsp.Vpc.Name)
					}
					if tc.expectetIPName != nil {
						assert.Equal(t, *tc.expectetIPName, rsp.IPBlock.Name)
					}
					if tc.expectUsageStatsNonNil {
						require.NotNil(t, rsp.IPBlock)
						require.NotNil(t, rsp.UsageStats)
					}
					if tc.expectUsageStatsNil {
						require.NotNil(t, rsp.IPBlock)
						assert.Nil(t, rsp.UsageStats)
					}
					if tc.verifyUsageAcquisitionFromIface {
						require.NotNil(t, rsp.UsageStats)
						assert.Greater(t, rsp.UsageStats.AcquiredPrefixes, uint64(0),
							"Ethernet interface IPv4 in vpc_prefix should consume at least one /31 via ephemeral IPAM")
						if rsp.UsageStats.AvailablePrefixes != nil {
							assert.LessOrEqual(t, len(rsp.UsageStats.AvailablePrefixes), 10)
						}
					}
				} else {
					assert.Nil(t, rsp.Vpc)
					assert.Nil(t, rsp.IPBlock)
				}
				if !expanded && !tc.expectedErr {
					assert.Nil(t, rsp.UsageStats)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}

		})
	}
}

func TestVpcPrefixHandler_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2, tnOrg3}, tnRoles)

	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)

	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site2)

	tenant1 := testMachineBuildTenant(t, dbSession, tnOrg1, "t1")
	tenant2 := testMachineBuildTenant(t, dbSession, tnOrg2, "t2")
	assert.NotNil(t, tenant2)

	vpc1 := testVpcPrefixBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site.ID.String()] = tsc
	scp.IDClientMap[site2.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	for _, tsc := range scp.IDClientMap {
		tsc.(*tmocks.Client).Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
			"UpdateVpcPrefix", mock.Anything).Return(wrun, nil)

		// They should all be the same client, so break after the first one.
		break
	}

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref1)
	prefixLen := 24
	vpcprefixBody, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "test-vpcprefix-1", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	vpcprefixBody2, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "test-vpcprefix-2", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	tscCallCount := 0

	vpcprefix := testCreateVpcPrefix(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(vpcprefixBody))
	tscCallCount++

	vpcprefix2 := testCreateVpcPrefix(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(vpcprefixBody2))
	tscCallCount++

	errBody1, err := json.Marshal(model.APIAllocationUpdateRequest{Name: cutil.GetPtr("a")})
	assert.Nil(t, err)

	errBodyNameClash, err := json.Marshal(model.APIVpcPrefixUpdateRequest{Name: cutil.GetPtr("test-vpcprefix-2")})
	assert.Nil(t, err)

	okBody1, err := json.Marshal(model.APIVpcPrefixUpdateRequest{Name: cutil.GetPtr("test-vpcprefix-updated-1")})
	assert.Nil(t, err)

	okBody3, err := json.Marshal(model.APIVpcPrefixUpdateRequest{Name: cutil.GetPtr("test-vpcprefix-2")})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		reqBody            string
		user               *cdbm.User
		id                 string
		expectedErr        bool
		expectedStatus     int
		expectedName       string
		expectedDesc       *string
		verifyChildSpanner bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody1),
			user:           nil,
			id:             vpcprefix.ID,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			reqBody:        string(okBody1),
			user:           tnu,
			id:             vpcprefix.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when reqBody doesnt bind",
			reqOrgName:     tnOrg1,
			reqBody:        "BadBody",
			user:           tnu,
			id:             vpcprefix.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when reqBody validation fails",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBody1),
			user:           tnu,
			id:             vpcprefix.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when vpcprefix id is invalid uuid",
			reqOrgName:     tnOrg1,
			user:           tnu,
			id:             "bad#uuid$str",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified org does not have tenant",
			reqOrgName:     tnOrg3,
			user:           tnu,
			id:             vpcprefix.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when vpcprefix tenant does not match org",
			reqOrgName:     tnOrg2,
			user:           tnu,
			id:             vpcprefix.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified vpcprefix doesnt exist",
			reqOrgName:     tnOrg1,
			user:           tnu,
			id:             uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when name clashes",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyNameClash),
			user:           tnu,
			id:             vpcprefix.ID,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:           "success when name is updated with non-clashing value",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody1),
			user:           tnu,
			id:             vpcprefix.ID,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedName:   "test-vpcprefix-updated-1",
			expectedDesc:   nil,
		},
		{
			name:           "success case when name is updated with the same value",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody3),
			user:           tnu,
			id:             vpcprefix2.ID,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedName:   "test-vpcprefix-2",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(tc.reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.id}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := UpdateVpcPrefixHandler{
				dbSession: dbSession,
				tc:        tempClient,
				scp:       scp,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			require.Equal(t, tc.expectedStatus, rec.Code, rec.Body.String())

			if tc.expectedErr {
				return
			}

			resp := &model.APIVpcPrefix{}
			err = json.Unmarshal(rec.Body.Bytes(), resp)
			assert.Nil(t, err)
			// validate response fields
			assert.Equal(t, 1, len(resp.StatusHistory))
			assert.Equal(t, tc.expectedName, resp.Name)

			assert.NotEqual(t, resp.Updated.String(), vpcprefix.Updated.String())

			require.Equal(t, tscCallCount+1, len(tsc.Calls))
			tscCallCount++
			require.Equal(t, "ExecuteWorkflow", tsc.Calls[len(tsc.Calls)-1].Method)
			require.Equal(t, 4, len(tsc.Calls[len(tsc.Calls)-1].Arguments))

			// Check Temporal workflow call arguments
			temporalReq, ok := tsc.Calls[len(tsc.Calls)-1].Arguments[3].(*corev1.VpcPrefixUpdateRequest)
			require.True(t, ok)
			assert.Equal(t, resp.Name, temporalReq.Metadata.Name)

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestVpcPrefixHandler_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2, tnOrg3}, tnRoles)

	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)
	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site2)

	tn1 := testMachineBuildTenant(t, dbSession, tnOrg1, "tenant-1")
	tn2 := testMachineBuildTenant(t, dbSession, tnOrg2, "tenant-2")

	_ = common.TestBuildTenantSite(t, dbSession, tn1, site, tnu)

	al1 := common.TestBuildAllocation(t, dbSession, site, tn1, "test-allocation-1", ipu)
	it1 := common.TestBuildInstanceType(t, dbSession, "test-instance-type-1", cutil.GetPtr(uuid.New()), site, nil, ipu)
	common.TestBuildAllocationConstraint(t, dbSession, al1, it1, nil, 5, ipu)

	m1 := common.TestBuildMachine(t, dbSession, ip, site, &it1.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)
	_ = common.TestBuildMachineInstanceType(t, dbSession, m1, it1)

	os1 := common.TestBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemStatusReady, tnu)
	vpc1 := testVpcPrefixBuildVpc(t, dbSession, ip, tn1, site, tnOrg1, "test-vpc-1", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))

	cfg := common.GetTestConfig()

	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls to Site
	tcfg, _ := cfg.GetTemporalConfig()

	// Temporal workflow timeout mocking
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[site.ID.String()] = tscWithTimeout
	scpWithTimeout.IDClientMap[site2.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteVpcPrefix", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// Temporal workflow Core gRPC error mock
	scpWithCoreGRPCError := sc.NewClientPool(tcfg)
	tscWithCoreGRPCError := &tmocks.Client{}

	scpWithCoreGRPCError.IDClientMap[site.ID.String()] = tscWithCoreGRPCError
	scpWithCoreGRPCError.IDClientMap[site2.ID.String()] = tscWithCoreGRPCError

	wrunWithCoreGRPCError := &tmocks.WorkflowRun{}
	wrunWithCoreGRPCError.On("GetID").Return("workflow-with-core-grpc-error")

	wrunWithCoreGRPCError.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("core gRPC failed precondition", swe.ErrTypeNICoFailedPrecondition, errors.New("core gRPC failed precondition")))

	tscWithCoreGRPCError.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteVpcPrefix", mock.Anything).Return(wrunWithCoreGRPCError, nil)

	tscWithCoreGRPCError.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// Temporal workflow success mocking
	tempClient := &tmocks.Client{}
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site.ID.String()] = tsc
	scp.IDClientMap[site2.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tempClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteVpcPrefix", mock.Anything).Return(wrun, nil)

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "test-ip-block-1", site, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref1)

	ipb2 := testIPBlockBuildIPBlock(t, dbSession, "test-ip-block-2", site2, ip2, &tn2.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	_, err = ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb2.Prefix, ipb2.PrefixLength, ipb2.RoutingType, ipb2.InfrastructureProviderID.String(), ipb2.SiteID.String())
	assert.Nil(t, err)

	ipbFG := testIPBlockBuildIPBlock(t, dbSession, "test-ip-block-full-grant", site, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.170.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	_, err = ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbFG.Prefix, ipbFG.PrefixLength, ipbFG.RoutingType, ipbFG.InfrastructureProviderID.String(), ipbFG.SiteID.String())
	assert.Nil(t, err)

	prefixLen := 24

	okBody, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "test-vpc-prefix-1", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	okBody2, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "test-vpc-prefix-2", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	okBody3, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "test-vpc-prefix-3", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	okBodyFG, err := json.Marshal(model.APIVpcPrefixCreateRequest{Name: "test-vpc-prefix-full-grant", VpcID: vpc1.ID.String(), IPBlockID: cutil.GetPtr(ipbFG.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	vpcp1 := testCreateVpcPrefix(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(okBody))

	vpcp2 := testCreateVpcPrefix(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(okBody2))
	vpcp2ID := uuid.MustParse(vpcp2.ID)
	vpcp3 := testCreateVpcPrefix(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(okBody3))
	vpcp3ID := uuid.MustParse(vpcp3.ID)

	vpcp1FG := testCreateVpcPrefix(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(okBodyFG))

	ins1 := common.TestBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, site.ID, it1.ID, vpc1.ID, cutil.GetPtr(m1.ID), os1.ID)
	dualStackInterface := common.TestBuildInterface(t, dbSession, ins1.ID, nil, &vpcp2ID, true, nil, nil, nil, cutil.GetPtr(cdbm.InterfaceStatusReady), tnu)
	_, err = cdbm.NewInterfaceDAO(dbSession).Update(ctx, nil, cdbm.InterfaceUpdateInput{
		InterfaceID:          dualStackInterface.ID,
		SecondaryVpcPrefixID: &vpcp3ID,
	})
	require.NoError(t, err)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		user               *cdbm.User
		id                 string
		allocation         *model.APIAllocation
		expectedErr        bool
		expectedStatus     int
		expectedErrorMsg   *string
		deleteInstanceID   *uuid.UUID
		deleteVpcPrefixID  *uuid.UUID
		verifyChildSpanner bool
		tClient            *tmocks.Client
		clientPool         *sc.ClientPool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			user:           nil,
			id:             vpcp1.ID,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           tnu,
			id:             vpcp1.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when vpcprefix id is invalid uuid",
			reqOrgName:     tnOrg1,
			user:           tnu,
			id:             "bad#uuid$str",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified org does not have tenant",
			reqOrgName:     tnOrg3,
			user:           tnu,
			id:             vpcp1.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified vpcprefix doesnt exist",
			reqOrgName:     tnOrg1,
			user:           tnu,
			id:             uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when vpcprefix tenant does not match org",
			reqOrgName:     tnOrg2,
			user:           tnu,
			id:             vpcp1.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "error when Temporal workflow timeout occurs",
			reqOrgName:         tnOrg1,
			user:               tnu,
			id:                 vpcp1.ID,
			expectedErr:        true,
			expectedStatus:     http.StatusInternalServerError,
			verifyChildSpanner: true,
			clientPool:         scpWithTimeout,
			tClient:            tscWithTimeout,
		},
		{
			name:               "error when Temporal workflow core gRPC precondition error occurs",
			reqOrgName:         tnOrg1,
			user:               tnu,
			id:                 vpcp1.ID,
			expectedErr:        true,
			expectedStatus:     http.StatusPreconditionFailed,
			verifyChildSpanner: true,
			clientPool:         scpWithCoreGRPCError,
			tClient:            tscWithCoreGRPCError,
		},
		{
			name:             "error when VPC Prefix is used by an Instance Interface",
			reqOrgName:       tnOrg1,
			user:             tnu,
			id:               vpcp2.ID,
			expectedErr:      true,
			expectedStatus:   http.StatusBadRequest,
			expectedErrorMsg: cutil.GetPtr("VPC Prefix is being used by one or more Instances and cannot be deleted"),
		},
		{
			name:             "error when VPC Prefix is used as a secondary dual-stack prefix",
			reqOrgName:       tnOrg1,
			user:             tnu,
			id:               vpcp3.ID,
			expectedErr:      true,
			expectedStatus:   http.StatusBadRequest,
			expectedErrorMsg: cutil.GetPtr("VPC Prefix is being used by one or more Instances and cannot be deleted"),
		},
		{
			name:               "success when deleting VPC Prefix",
			reqOrgName:         tnOrg1,
			user:               tnu,
			id:                 vpcp1.ID,
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
		{
			name:           "success when deleting VPC Prefix with full grant",
			reqOrgName:     tnOrg1,
			user:           tnu,
			id:             vpcp1FG.ID,
			expectedErr:    false,
			expectedStatus: http.StatusAccepted,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.id}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			tClient := tempClient
			if tc.tClient != nil {
				tClient = tc.tClient
			}

			clientPool := scp
			if tc.clientPool != nil {
				clientPool = tc.clientPool
			}

			dsh := DeleteVpcPrefixHandler{
				dbSession: dbSession,
				tc:        tClient,
				scp:       clientPool,
				cfg:       cfg,
			}
			err := dsh.Handle(ec)
			assert.Nil(t, err)

			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusAccepted)
			assert.Equal(t, tc.expectedStatus, rec.Code)

			if tc.expectedErr {
				if tc.expectedErrorMsg != nil {
					assert.Contains(t, rec.Body.String(), *tc.expectedErrorMsg)
				}
			} else {
				vpDAO := cdbm.NewVpcPrefixDAO(dbSession)
				id1, err := uuid.Parse(tc.id)
				assert.Nil(t, err)
				usb, err := vpDAO.GetByID(ctx, nil, id1, nil)
				assert.Nil(t, err)
				assert.Equal(t, cdbm.VpcPrefixStatusDeleting, usb.Status)
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
