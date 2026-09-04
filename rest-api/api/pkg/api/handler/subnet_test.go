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
)

func testSubnetBuildDomain(t *testing.T, dbSession *cdb.Session, hostname, org string, userID *uuid.UUID) *cdbm.Domain {
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

func testSubnetBuildInterface(t *testing.T, dbSession *cdb.Session, instanceID, subnetID, userID *uuid.UUID) *cdbm.Interface {
	is := &cdbm.Interface{
		ID:         uuid.New(),
		InstanceID: *instanceID,
		SubnetID:   subnetID,
		Status:     cdbm.InterfaceStatusPending,
		CreatedBy:  *userID,
	}
	_, err := dbSession.DB.NewInsert().Model(is).Exec(context.Background())
	assert.Nil(t, err)
	return is
}

func testSubnetCIDREntity(t *testing.T, sn *cdbm.Subnet) string {
	t.Helper()
	require.NotNil(t, sn)
	require.NotNil(t, sn.IPv4Prefix)
	p := strings.TrimSpace(*sn.IPv4Prefix)
	require.NotEmpty(t, p)
	if strings.Contains(p, "/") {
		return p
	}
	return fmt.Sprintf("%s/%d", p, sn.PrefixLength)
}

func testSubnetBuildVpc(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, tenant *cdbm.Tenant, site *cdbm.Site, org, name string, networkVtType *string, status string, controllerVpcID *uuid.UUID) *cdbm.Vpc {
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

func TestSubnetHandler_Create(t *testing.T) {
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
	vpc1 := testSubnetBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc2 := testSubnetBuildVpc(t, dbSession, ip, tenant1, site2, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc3 := testSubnetBuildVpc(t, dbSession, ip, tenant1, site2, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc4 := testSubnetBuildVpc(t, dbSession, ip, tenant1, site2, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusPending, nil)
	vpc5 := testSubnetBuildVpc(t, dbSession, ip, tenant1, site3, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc6 := testSubnetBuildVpc(t, dbSession, ip, tenant2, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc7 := testSubnetBuildVpc(t, dbSession, ip, tenant2, site4, tnOrg2, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc8 := testSubnetBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcFNN), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpcLegacy := testSubnetBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "legacyVPC", nil, cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	buildDomain := func(hostname string, tenant *cdbm.Tenant, domainSite *cdbm.Site, status string, controllerDomainID *uuid.UUID) *cdbm.Domain {
		domain := &cdbm.Domain{
			ID: uuid.New(), Hostname: hostname, Org: tenant.Org, TenantID: &tenant.ID, SiteID: &domainSite.ID,
			ControllerDomainID: controllerDomainID, Status: status, CreatedBy: tnu.ID,
		}
		_, createErr := dbSession.DB.NewInsert().Model(domain).Exec(ctx)
		require.NoError(t, createErr)
		return domain
	}
	controllerDomainID := uuid.New()
	domain := buildDomain("ready.example.com", tenant1, site, cdbm.DomainStatusReady, &controllerDomainID)
	otherTenantDomain := buildDomain("other-tenant.example.com", tenant2, site, cdbm.DomainStatusReady, cutil.GetPtr(uuid.New()))
	otherSiteDomain := buildDomain("other-site.example.com", tenant1, site2, cdbm.DomainStatusReady, cutil.GetPtr(uuid.New()))
	pendingDomain := buildDomain("pending.example.com", tenant1, site, cdbm.DomainStatusPending, cutil.GetPtr(uuid.New()))
	domainWithoutControllerID := buildDomain("unregistered.example.com", tenant1, site, cdbm.DomainStatusReady, nil)

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
		"CreateSubnetV2", mock.Anything).Return(wrun, nil)

	// Mock timeout error
	tsc1 := &tmocks.Client{}
	scp.IDClientMap[site4.ID.String()] = tsc1

	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")

	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tsc1.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateSubnetV2", mock.Anything).Return(wruntimeout, nil)

	tsc1.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref1)
	ipbV6 := testIPBlockBuildIPBlock(t, dbSession, "testipbv6", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "2001:db8::", 64, cdbm.IPBlockProtocolVersionV6, false, cdbm.IPBlockStatusReady, ipu)
	// Do not create an IPAM parent for this Pending block. Its test must fail
	// during source-block validation.
	ipbPending := testIPBlockBuildIPBlock(t, dbSession, "testipbpending", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.172.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusPending, ipu)
	ipbLegacy := testIPBlockBuildIPBlock(t, dbSession, "testipblegacy", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.173.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPrefLegacy, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbLegacy.Prefix, ipbLegacy.PrefixLength, ipbLegacy.RoutingType, ipbLegacy.InfrastructureProviderID.String(), ipbLegacy.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPrefLegacy)

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

	okBodyTimeout, err := json.Marshal(model.APISubnetCreateRequest{Name: "oktimeout", Description: cutil.GetPtr(""), VpcID: vpc7.ID.String(), IPv4BlockID: cutil.GetPtr(ipb4.ID.String()), PrefixLength: 24})
	assert.Nil(t, err)

	ipbFG := testIPBlockBuildIPBlock(t, dbSession, "testipbfg", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.170.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPrefFG, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbFG.Prefix, ipbFG.PrefixLength, ipbFG.RoutingType, ipbFG.InfrastructureProviderID.String(), ipbFG.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPrefFG)
	// This record belongs to the same tenant as the valid block. SitePrefixID is
	// what makes it a SitePrefix rather than an Allocation IPBlock.
	tenantSitePrefix := testIPBlockBuildTenantSitePrefix(t, dbSession, "private-site-prefix", site, ip, tenant1, "192.171.0.0", 16, cdbm.IPBlockStatusReady, ipu)
	prefixLen := 24
	okBody, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	okBodyWithDomain, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok-domain", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), SubdomainID: cutil.GetPtr(domain.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyMissingDomain, err := json.Marshal(model.APISubnetCreateRequest{Name: "missing-domain", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), SubdomainID: cutil.GetPtr(uuid.NewString()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyOtherTenantDomain, err := json.Marshal(model.APISubnetCreateRequest{Name: "other-tenant-domain", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), SubdomainID: cutil.GetPtr(otherTenantDomain.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyOtherSiteDomain, err := json.Marshal(model.APISubnetCreateRequest{Name: "other-site-domain", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), SubdomainID: cutil.GetPtr(otherSiteDomain.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyPendingDomain, err := json.Marshal(model.APISubnetCreateRequest{Name: "pending-domain", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), SubdomainID: cutil.GetPtr(pendingDomain.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyDomainWithoutControllerID, err := json.Marshal(model.APISubnetCreateRequest{Name: "domain-without-controller-id", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), SubdomainID: cutil.GetPtr(domainWithoutControllerID.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	okBodyLegacyVpc, err := json.Marshal(model.APISubnetCreateRequest{Name: "legacy-vpc", Description: cutil.GetPtr(""), VpcID: vpcLegacy.ID.String(), IPv4BlockID: cutil.GetPtr(ipbLegacy.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyPendingIPv4Block, err := json.Marshal(model.APISubnetCreateRequest{Name: "pending-ip-block", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipbPending.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	prefixLen = 16
	okBodyFG, err := json.Marshal(model.APISubnetCreateRequest{Name: "okFG", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipbFG.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	prefixLen = 30
	okBodySlash30, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok30", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	prefixLen = 31
	errBodySlash31, err := json.Marshal(model.APISubnetCreateRequest{Name: "err31", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	prefixLen = 24
	okBodyNameClash, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc2.ID.String(), IPv4BlockID: cutil.GetPtr(ipb3.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyNameClash, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	errBodyDoesntValidate, err := json.Marshal(struct{ Name string }{Name: "test"})
	assert.Nil(t, err)
	errBodyBadVpcID, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: uuid.New().String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyBadVpcTenantMismatch, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc6.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyBadVpctype, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc8.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyBadIPv4BlockID, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyIPv6AsIPv4Block, err := json.Marshal(model.APISubnetCreateRequest{Name: "ipv6-as-ipv4", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipbV6.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyTenantSitePrefixID, err := json.Marshal(model.APISubnetCreateRequest{Name: "private-prefix", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(tenantSitePrefix.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyNoIPv4, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), PrefixLength: prefixLen})
	assert.Nil(t, err)
	errBodyBadIPv6BlockID, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), IPv6BlockID: cutil.GetPtr(uuid.New().String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	errBodyBadIPv4BlockIDTenantMismatch, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb2.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	errBodyBadIPv4BlockIDSiteMismatch, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc3.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	prefixLen = 15
	errBodyIpamFail, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)
	prefixLen = 24
	errVpcNotReady, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok4", Description: cutil.GetPtr(""), VpcID: vpc4.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	errSiteNotReady, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok5", Description: cutil.GetPtr(""), VpcID: vpc5.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
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
		expectedGateway    string
		expectedPrefix     string
		expectedSubdomain  *uuid.UUID
		expectedCoreDomain *uuid.UUID
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
			name:           "REST Subnet creation rejects an FNN VPC",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyBadVpctype),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "REST Subnet creation rejects ipv6BlockId",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyBadIPv6BlockID),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "REST Subnet creation requires ipv4BlockId",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyNoIPv4),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when ipv4 block in request doesnt exist",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyBadIPv4BlockID),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "Pending tenant IPv4 block is rejected before IPAM allocation",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyPendingIPv4Block),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedErrMsg: "Could not find a Ready, tenant-allocated IPv4 IP block specified by ipv4BlockId",
		},
		{
			name:           "ipv4BlockId rejects an IPv6 IP block",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyIPv6AsIPv4Block),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedErrMsg: "ipv4BlockId must reference an IPv4 IP block for Subnet creation",
		},
		{
			name:           "Tenant SitePrefix cannot be the source IP Block for a Subnet",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyTenantSitePrefixID),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedErrMsg: "Could not find a Ready, tenant-allocated IPv4 IP block specified by ipv4BlockId",
		},
		{
			name:           "error when ipv4 block in request is not derived for tenant",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyBadIPv4BlockIDTenantMismatch),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when site is mismatched between ipblock and site",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyBadIPv4BlockIDSiteMismatch),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when ipam creation for subnet fails",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyIpamFail),
			user:           tnu,
			expectedErr:    true,
			expectedErrMsg: "Could not create IPAM entry for Subnet. Details: given length:15 must be greater than prefix length:16",
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:            "success case",
			reqOrgName:      tnOrg1,
			reqBody:         string(okBody),
			user:            tnu,
			expectedErr:     false,
			expectedStatus:  http.StatusCreated,
			expectedGateway: "192.168.0.1",
			expectedPrefix:  "192.168.0.0",
		},
		{
			name:               "successfully resolves a tenant Site Domain for Core",
			reqOrgName:         tnOrg1,
			reqBody:            string(okBodyWithDomain),
			user:               tnu,
			expectedStatus:     http.StatusCreated,
			expectedGateway:    "192.168.1.1",
			expectedPrefix:     "192.168.1.0",
			expectedSubdomain:  &domain.ID,
			expectedCoreDomain: &controllerDomainID,
		},
		{
			name:           "rejects a missing Domain",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyMissingDomain),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "rejects another Tenant's Domain",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyOtherTenantDomain),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "rejects a Domain at another Site",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyOtherSiteDomain),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "rejects a Domain that is not Ready",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyPendingDomain),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "rejects a Domain without a Core ID",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyDomainWithoutControllerID),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:            "Ready legacy VPC without networkVirtualizationType remains compatible",
			reqOrgName:      tnOrg1,
			reqBody:         string(okBodyLegacyVpc),
			user:            tnu,
			expectedErr:     false,
			expectedStatus:  http.StatusCreated,
			expectedGateway: "192.173.0.1",
			expectedPrefix:  "192.173.0.0",
		},
		{
			name:               "success case with Full Grant",
			reqOrgName:         tnOrg1,
			reqBody:            string(okBodyFG),
			user:               tnu,
			expectedErr:        false,
			expectedStatus:     http.StatusCreated,
			expectedGateway:    "192.170.0.1",
			expectedPrefix:     "192.170.0.0",
			verifyChildSpanner: true,
		},
		{
			name:            "success case with /30",
			reqOrgName:      tnOrg1,
			reqBody:         string(okBodySlash30),
			user:            tnu,
			expectedErr:     false,
			expectedStatus:  http.StatusCreated,
			expectedGateway: "192.168.2.1",
			expectedPrefix:  "192.168.2.0",
		},
		{
			name:           "error case with /31",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodySlash31),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when subnet with same name already exists",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyNameClash),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:            "success when Subnet with same name exists, but for another Site",
			reqOrgName:      tnOrg1,
			reqBody:         string(okBodyNameClash),
			user:            tnu,
			expectedErr:     false,
			expectedStatus:  http.StatusCreated,
			expectedGateway: "192.170.0.1",
			expectedPrefix:  "192.170.0.0",
		},
		{
			name:           "error when vpc from subnet is not ready state",
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
			name:           "subnet creation fails, sync workflow timeout",
			reqOrgName:     tnOrg2,
			reqBody:        string(okBodyTimeout),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
	}
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

			cipbh := CreateSubnetHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}
			err := cipbh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusCreated)
			assert.Equal(t, tc.expectedStatus, rec.Code)

			if tc.expectedStatus != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}

			if !tc.expectedErr {
				rsp := &model.APISubnet{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, len(rsp.StatusHistory), 1)
				// validate prefix
				assert.NotNil(t, rsp.IPv4Prefix)
				assert.Equal(t, tc.expectedPrefix, *rsp.IPv4Prefix)
				// validate gateway
				assert.NotNil(t, rsp.IPv4Gateway)
				assert.Equal(t, tc.expectedGateway, *rsp.IPv4Gateway)

				assert.Equal(t, cdbm.IPBlockRoutingTypeDatacenterOnly, *rsp.RoutingType)
				if tc.expectedSubdomain != nil {
					require.NotNil(t, rsp.SubdomainID)
					assert.Equal(t, tc.expectedSubdomain.String(), *rsp.SubdomainID)
					persistedSubnet, getErr := cdbm.NewSubnetDAO(dbSession).GetByID(ctx, nil, uuid.MustParse(rsp.ID), nil)
					require.NoError(t, getErr)
					require.NotNil(t, persistedSubnet.DomainID)
					assert.Equal(t, *tc.expectedSubdomain, *persistedSubnet.DomainID)
				}
				if tc.expectedCoreDomain != nil {
					require.NotEmpty(t, tsc.Calls)
					workflowRequest := tsc.Calls[len(tsc.Calls)-1].Arguments.Get(3).(*corev1.NetworkSegmentCreationRequest)
					require.NotNil(t, workflowRequest.SubdomainId)
					assert.Equal(t, tc.expectedCoreDomain.String(), workflowRequest.SubdomainId.Value)
				}

				// validate ipam exists for subnet
				if tc.expectedIpam {
					parentIPBID := rsp.IPv4BlockID
					parentIPBUUID, err := uuid.Parse(*parentIPBID)
					assert.Nil(t, err)
					ipbDAO := cdbm.NewIPBlockDAO(dbSession)
					parentIPB, err := ipbDAO.GetByID(ctx, nil, parentIPBUUID, nil)
					assert.Nil(t, err)
					ipamer := cipam.NewWithStorage(ipamStorage)
					ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(ctx, parentIPB.RoutingType, parentIPB.InfrastructureProviderID.String(), parentIPB.SiteID.String()))
					pref := ipamer.PrefixFrom(ctx, ipam.GetCidrForIPBlock(ctx, *rsp.IPv4Prefix, rsp.PrefixLength))
					assert.NotNil(t, pref)
				}
			} else {
				if tc.expectedErrMsg != "" {
					var apiErr struct {
						Message string `json:"message"`
					}
					require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiErr))
					assert.Equal(t, tc.expectedErrMsg, apiErr.Message)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func testCreateSubnet(t *testing.T, dbSession *cdb.Session, scp *sc.ClientPool, ipamStorage cipam.Storage, user *cdbm.User, reqOrgName, reqBody string) *model.APISubnet {
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
			"CreateSubnetV2", mock.Anything).Return(wrun, nil)

		// They should all be the same client, so break after the first one.
		break
	}

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	cipbh := CreateSubnetHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       common.GetTestConfig(),
		scp:       scp,
	}
	err := cipbh.Handle(ec)
	assert.Nil(t, err)
	assert.Equal(t, rec.Code, http.StatusCreated)
	rsp := &model.APISubnet{}
	err = json.Unmarshal(rec.Body.Bytes(), rsp)
	assert.Nil(t, err)
	return rsp
}

func TestSubnetHandler_GetAll(t *testing.T) {
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

	vpc1 := testSubnetBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	vpc2 := testSubnetBuildVpc(t, dbSession, ip, tenant2, site2, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
	assert.NotNil(t, vpc2)
	vpc3 := testSubnetBuildVpc(t, dbSession, ip, tenant1, site2, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))
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

	sDAO := cdbm.NewSubnetDAO(dbSession)
	ss := []cdbm.Subnet{}

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
		subnetBody, err := json.Marshal(model.APISubnetCreateRequest{Name: fmt.Sprintf("subnet-%02d", i), Description: cutil.GetPtr(""), VpcID: vpcID.String(), IPv4BlockID: cutil.GetPtr(ipbID.String()), PrefixLength: prefixLen})
		assert.Nil(t, err)
		apiSubnet := testCreateSubnet(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(subnetBody))
		assert.NotNil(t, apiSubnet)

		sid, _ := uuid.Parse(apiSubnet.ID)
		s, err := sDAO.GetByID(ctx, nil, sid, nil)
		assert.Nil(t, err)
		common.TestBuildStatusDetail(t, dbSession, sid.String(), cdbm.SubnetStatusReady, cutil.GetPtr("Subnet has been provisioned on Site"))
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
		queryStatus            *string
		pageNumber             *int
		pageSize               *int
		orderBy                *string
		expectedErr            bool
		expectedStatus         int
		expectedCnt            int
		expectedTotal          *int
		expectedFirstEntry     *cdbm.Subnet
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
			name:                   "success when include Ipv4 relation specified",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedCnt:            paginator.DefaultLimit,
			expectedTotal:          &totalCount,
			queryIncludeRelations1: cutil.GetPtr(cdbm.IPv4BlockRelationName),
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
			querySearch:    cutil.GetPtr("subnet"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when query search status specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr("pending"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when query search name and status specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr("notexisted pending"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when Pending status specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryStatus:    cutil.GetPtr(cdbm.SubnetStatusPending),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when BadStatus status specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryStatus:    cutil.GetPtr("BadStatus"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
			expectedTotal:  cutil.GetPtr(0),
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
			if tc.queryStatus != nil {
				q.Add("status", *tc.queryStatus)
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

			path := fmt.Sprintf("/v2/org/%s/nico/subnet?%s", tc.reqOrgName, q.Encode())

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

			gash := GetAllSubnetHandler{
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

			resp := []model.APISubnet{}
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
				assert.Equal(t, cdbm.IPBlockRoutingTypeDatacenterOnly, *resp[0].RoutingType)
			}

			if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil || tc.queryIncludeRelations3 != nil || tc.queryIncludeRelations4 != nil {
				if tc.expectedVpcName != nil {
					assert.Equal(t, *tc.expectedVpcName, resp[0].Vpc.Name)
				}
				if tc.expectetIPv4Name != nil {
					assert.Equal(t, *tc.expectetIPv4Name, resp[0].IPv4Block.Name)
				}
				if tc.expectedIPv6Name != nil {
					assert.Equal(t, *tc.expectedIPv6Name, resp[0].IPv6Block.Name)
				}
				if tc.expectedSiteName != nil {
					assert.Equal(t, *tc.expectedSiteName, resp[0].Site.Name)
				}
			} else {
				if len(resp) > 0 {
					assert.Nil(t, resp[0].Vpc)
					assert.Nil(t, resp[0].IPv4Block)
					assert.Nil(t, resp[0].IPv6Block)
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

func TestSubnetHandler_Get(t *testing.T) {
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
	vpc1 := testSubnetBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))

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
	parentIpbBody, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	subnet := testCreateSubnet(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(parentIpbBody))

	ifaceSubnetBody, err := json.Marshal(model.APISubnetCreateRequest{
		Name: "iface-subnet-usage", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(),
		IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	require.NoError(t, err)
	subnetWithIfaceWorkload := testCreateSubnet(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(ifaceSubnetBody))
	var wantUsageAcquiredPrefixes0 uint64
	wantUsageAcquiredPrefixes1 := uint64(0)

	alWorkload := common.TestBuildAllocation(t, dbSession, site, tenant1, "get-subnet-usage-iface-alloc", ipu)
	itWorkload := common.TestBuildInstanceType(t, dbSession, "get-subnet-iface-it", cutil.GetPtr(uuid.New()), site, nil, ipu)
	common.TestBuildAllocationConstraint(t, dbSession, alWorkload, itWorkload, nil, 5, ipu)
	mWorkload := common.TestBuildMachine(t, dbSession, ip, site, &itWorkload.ID, cutil.GetPtr("get-subnet-iface-mt"), cdbm.MachineStatusReady)
	_ = common.TestBuildMachineInstanceType(t, dbSession, mWorkload, itWorkload)
	osWorkload := common.TestBuildOperatingSystem(t, dbSession, "get-subnet-iface-os", tenant1, cdbm.OperatingSystemStatusReady, tnu)

	snWorkloadUUID := uuid.MustParse(subnetWithIfaceWorkload.ID)
	snEnt, err := cdbm.NewSubnetDAO(dbSession).GetByID(ctx, nil, snWorkloadUUID, nil)
	require.NoError(t, err)
	cidrWorkload := testSubnetCIDREntity(t, snEnt)
	chosenIfaceIPv4 := testIPv4NthUsableInCIDR(t, cidrWorkload, 20)

	instWorkload := common.TestBuildInstance(t, dbSession, "get-subnet-iface-inst", tenant1.ID, ip.ID, site.ID, itWorkload.ID, vpc1.ID, cutil.GetPtr(mWorkload.ID), osWorkload.ID)
	ifWorkload := common.TestBuildInterface(t, dbSession, instWorkload.ID, &snWorkloadUUID, nil, true, nil, nil, nil, cutil.GetPtr(cdbm.InterfaceStatusReady), tnu)
	_, err = cdbm.NewInterfaceDAO(dbSession).Update(ctx, nil, cdbm.InterfaceUpdateInput{
		InterfaceID: ifWorkload.ID,
		IpAddresses: []string{chosenIfaceIPv4},
	})
	require.NoError(t, err)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                   string
		reqOrgName             string
		user                   *cdbm.User
		id                     string
		expectedErr            bool
		expectedStatus         int
		queryIncludeRelations1 *string
		queryIncludeRelations2 *string
		queryIncludeRelations3 *string
		queryIncludeUsageStats *string
		expectedVpcName        *string
		expectetIPv4Name       *string
		expectedIPv6Name       *string
		expectUsageStatsNonNil bool
		wantAcquiredPrefixes   *uint64
		verifyChildSpanner     bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			user:           nil,
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           tnu,
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when subnet id is invalid uuid",
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
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when subnet tenant does not match org",
			reqOrgName:     tnOrg2,
			user:           tnu,
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified subnet doesnt exist",
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
			id:             subnet.ID,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
		},
		{
			name:                   "success case when include VPC/Tenant relation",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			id:                     subnet.ID,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			queryIncludeRelations1: cutil.GetPtr(cdbm.VpcRelationName),
			expectedVpcName:        &vpc1.Name,
			verifyChildSpanner:     true,
		},
		{
			name:                   "success case when include IPv4 relation",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			id:                     subnet.ID,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			queryIncludeRelations1: cutil.GetPtr(cdbm.IPv4BlockRelationName),
			expectetIPv4Name:       &ipb1.Name,
		},
		{
			name:                   "error when includeUsageStats query invalid",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			id:                     subnet.ID,
			expectedErr:            true,
			expectedStatus:         http.StatusBadRequest,
			queryIncludeUsageStats: cutil.GetPtr("not-a-bool"),
		},
		{
			name:                   "success when includeUsageStats true and no ethernet interfaces",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			id:                     subnet.ID,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			queryIncludeUsageStats: cutil.GetPtr("true"),
			expectUsageStatsNonNil: true,
			wantAcquiredPrefixes:   &wantUsageAcquiredPrefixes0,
		},
		{
			name:                   "success when includeUsageStats true with ethernet interface and IPv4 IPs",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			id:                     subnetWithIfaceWorkload.ID,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			queryIncludeUsageStats: cutil.GetPtr("true"),
			expectUsageStatsNonNil: true,
			wantAcquiredPrefixes:   &wantUsageAcquiredPrefixes1,
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

			tah := GetSubnetHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APISubnet{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, 1, len(rsp.StatusHistory))

				assert.Equal(t, cdbm.IPBlockRoutingTypeDatacenterOnly, *rsp.RoutingType)

				hasRelations := tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil || tc.queryIncludeRelations3 != nil
				hasUsageStats := tc.expectUsageStatsNonNil
				if hasRelations || hasUsageStats {
					if tc.expectedVpcName != nil {
						assert.Equal(t, *tc.expectedVpcName, rsp.Vpc.Name)
					}
					if tc.expectetIPv4Name != nil {
						assert.Equal(t, *tc.expectetIPv4Name, rsp.IPv4Block.Name)
					}
					if tc.expectedIPv6Name != nil {
						assert.Equal(t, *tc.expectedIPv6Name, rsp.IPv6Block.Name)
					}
					if hasUsageStats {
						require.NotNil(t, rsp.IPv4Block)
					}
				} else {
					assert.Nil(t, rsp.Vpc)
					assert.Nil(t, rsp.IPv4Block)
					assert.Nil(t, rsp.IPv6Block)
				}
				if tc.expectUsageStatsNonNil {
					require.NotNil(t, rsp.UsageStats)
				} else {
					assert.Nil(t, rsp.UsageStats)
				}
				if tc.wantAcquiredPrefixes != nil {
					require.NotNil(t, rsp.UsageStats)
					assert.Equal(t, *tc.wantAcquiredPrefixes, rsp.UsageStats.AcquiredPrefixes)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}

		})
	}
}

func TestSubnetHandler_Update(t *testing.T) {
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

	vpc1 := testSubnetBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))

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
	subnetBody, err := json.Marshal(model.APISubnetCreateRequest{Name: "test-subnet-1", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	subnetBody2, err := json.Marshal(model.APISubnetCreateRequest{Name: "test-subnet-2", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	subnet := testCreateSubnet(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(subnetBody))

	subnet2 := testCreateSubnet(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(subnetBody2))
	assert.NotNil(t, subnet2)

	errBody1, err := json.Marshal(model.APIAllocationUpdateRequest{Name: cutil.GetPtr("a")})
	assert.Nil(t, err)

	errBodyNameClash, err := json.Marshal(model.APISubnetUpdateRequest{Name: cutil.GetPtr("test-subnet-2")})
	assert.Nil(t, err)

	okBody1, err := json.Marshal(model.APISubnetUpdateRequest{Name: cutil.GetPtr("test-subnet-updated-1")})
	assert.Nil(t, err)
	okBody2, err := json.Marshal(model.APISubnetUpdateRequest{Name: cutil.GetPtr("test-subnet-updated-2"), Description: cutil.GetPtr("Updated Description 2")})
	assert.Nil(t, err)
	okBody3, err := json.Marshal(model.APISubnetUpdateRequest{Name: cutil.GetPtr("test-subnet-2")})
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
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			reqBody:        string(okBody1),
			user:           tnu,
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when reqBody doesnt bind",
			reqOrgName:     tnOrg1,
			reqBody:        "BadBody",
			user:           tnu,
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when reqBody validation fails",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBody1),
			user:           tnu,
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when subnet id is invalid uuid",
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
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when subnet tenant does not match org",
			reqOrgName:     tnOrg2,
			user:           tnu,
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified subnet doesnt exist",
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
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:           "success when name is updated with non-clashing value",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody1),
			user:           tnu,
			id:             subnet.ID,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedName:   "test-subnet-updated-1",
			expectedDesc:   nil,
		},
		{
			name:               "success case when name and description are updated",
			reqOrgName:         tnOrg1,
			reqBody:            string(okBody2),
			user:               tnu,
			id:                 subnet.ID,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedName:       "test-subnet-updated-2",
			expectedDesc:       cutil.GetPtr("Updated Description 2"),
			verifyChildSpanner: true,
		},
		{
			name:           "success case when name is updated with the same value",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody3),
			user:           tnu,
			id:             subnet2.ID,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedName:   "test-subnet-2",
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

			tah := UpdateSubnetHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APISubnet{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, 1, len(rsp.StatusHistory))
				assert.Equal(t, tc.expectedName, rsp.Name)
				if tc.expectedDesc != nil {
					assert.Equal(t, *tc.expectedDesc, *rsp.Description)
				}
				assert.NotEqual(t, rsp.Updated.String(), subnet.Updated.String())
				assert.Equal(t, cdbm.IPBlockRoutingTypeDatacenterOnly, *rsp.RoutingType)
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSubnetHandler_Delete(t *testing.T) {
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

	vpc1 := testSubnetBuildVpc(t, dbSession, ip, tenant1, site, tnOrg1, "testVPC", cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cdbm.VpcStatusReady, cutil.GetPtr(uuid.New()))

	cfg := common.GetTestConfig()
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[site.ID.String()] = tscWithTimeout
	scpWithTimeout.IDClientMap[site2.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteSubnetV2", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo not-found mocking
	//
	scpWithNICoNotFound := sc.NewClientPool(tcfg)
	tscWithNICoNotFound := &tmocks.Client{}

	scpWithNICoNotFound.IDClientMap[site.ID.String()] = tscWithNICoNotFound
	scpWithNICoNotFound.IDClientMap[site2.ID.String()] = tscWithNICoNotFound

	wrunWithNICoNotFound := &tmocks.WorkflowRun{}
	wrunWithNICoNotFound.On("GetID").Return("workflow-WithNICoNotFound")

	wrunWithNICoNotFound.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoObjectNotFound, errors.New("NICo went bananas")))

	tscWithNICoNotFound.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteSubnetV2", mock.Anything).Return(wrunWithNICoNotFound, nil)

	tscWithNICoNotFound.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// Normal mocks
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
		"DeleteSubnetV2", mock.Anything).Return(wrun, nil)

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref1)
	ipb2 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site2, ip2, &tenant2.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPref2, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb2.Prefix, ipb2.PrefixLength, ipb2.RoutingType, ipb2.InfrastructureProviderID.String(), ipb2.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref2)

	ipbFG := testIPBlockBuildIPBlock(t, dbSession, "testipbfg", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.170.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	parentPrefFG, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbFG.Prefix, ipbFG.PrefixLength, ipbFG.RoutingType, ipbFG.InfrastructureProviderID.String(), ipbFG.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPrefFG)
	prefixLen := 24
	okBody, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	okBody2, err := json.Marshal(model.APISubnetCreateRequest{Name: "ok2", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipb1.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	okBodyFG, err := json.Marshal(model.APISubnetCreateRequest{Name: "okFG", Description: cutil.GetPtr(""), VpcID: vpc1.ID.String(), IPv4BlockID: cutil.GetPtr(ipbFG.ID.String()), PrefixLength: prefixLen})
	assert.Nil(t, err)

	subnet := testCreateSubnet(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(okBody))
	subnet2 := testCreateSubnet(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(okBody2))
	subnetFG := testCreateSubnet(t, dbSession, scp, ipamStorage, tnu, tnOrg1, string(okBodyFG))
	subnet2ID := uuid.MustParse(subnet2.ID)

	it1 := testMachineBuildInstanceType(t, dbSession, ip, site, "testIT")

	// build some machines, and map the machines to the instancetypes
	for i := 1; i <= 3; i++ {
		mc := testInstanceBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(false), nil)
		assert.NotNil(t, mc)
		mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc, it1)
		assert.NotNil(t, mcinst1)
	}

	acGoodIT := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 2}
	okBodyIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	aIT := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIT))
	_ = uuid.MustParse(aIT.ID)
	_ = uuid.MustParse(aIT.AllocationConstraints[0].ID)
	os1 := testAllocationBuildOperatingSystem(t, dbSession, "ubuntu")
	instanceDAO := cdbm.NewInstanceDAO(dbSession)
	instance, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "testInst",
			TenantID:                 tenant1.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &it1.ID,
			VpcID:                    vpc1.ID,
			OperatingSystemID:        cutil.GetPtr(os1.ID),
			Status:                   cdbm.InstanceStatusReady,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	instance2, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "testInst2",
			TenantID:                 tenant1.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &it1.ID,
			VpcID:                    vpc1.ID,
			OperatingSystemID:        cutil.GetPtr(os1.ID),
			Status:                   cdbm.InstanceStatusReady,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	ifc := testSubnetBuildInterface(t, dbSession, &instance.ID, &subnet2ID, &tnu.ID)
	assert.NotNil(t, ifc)
	ifc2 := testSubnetBuildInterface(t, dbSession, &instance2.ID, &subnet2ID, &tnu.ID)
	assert.NotNil(t, ifc2)

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
		deleteInstanceID   *uuid.UUID
		deleteSubnetID     *uuid.UUID
		verifyChildSpanner bool
		tClient            *tmocks.Client
		clientPool         *sc.ClientPool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			user:           nil,
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           tnu,
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when subnet id is invalid uuid",
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
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified subnet doesnt exist",
			reqOrgName:     tnOrg1,
			user:           tnu,
			id:             uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when subnet tenant does not match org",
			reqOrgName:     tnOrg2,
			user:           tnu,
			id:             subnet.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant has instances using subnet",
			reqOrgName:     tnOrg1,
			user:           tnu,
			id:             subnet2.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "workflow timeout failure",
			reqOrgName:         tnOrg1,
			user:               tnu,
			id:                 subnet.ID,
			expectedErr:        true,
			expectedStatus:     http.StatusInternalServerError,
			verifyChildSpanner: true,
			clientPool:         scpWithTimeout,
			tClient:            tscWithTimeout,
		},
		{
			name:               "nico not-found success",
			reqOrgName:         tnOrg1,
			user:               tnu,
			id:                 subnet.ID,
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
			clientPool:         scpWithNICoNotFound,
			tClient:            tscWithNICoNotFound,
		},
		{
			name:               "success case",
			reqOrgName:         tnOrg1,
			user:               tnu,
			id:                 subnet.ID,
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
		{
			name:           "success case with full grant",
			reqOrgName:     tnOrg1,
			user:           tnu,
			id:             subnetFG.ID,
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

			dsh := DeleteSubnetHandler{
				dbSession: dbSession,
				tc:        tClient,
				scp:       clientPool,
				cfg:       cfg,
			}
			err := dsh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusAccepted)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				sDAO := cdbm.NewSubnetDAO(dbSession)
				id1, err := uuid.Parse(tc.id)
				assert.Nil(t, err)
				usb, err := sDAO.GetByID(ctx, nil, id1, nil)
				assert.Nil(t, err)
				assert.Equal(t, cdbm.SubnetStatusDeleting, usb.Status)
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
