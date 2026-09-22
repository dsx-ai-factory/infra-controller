// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"

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
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	oteltrace "go.opentelemetry.io/otel/trace"
	tmocks "go.temporal.io/sdk/mocks"
)

func testAllocationBuildOperatingSystem(t *testing.T, dbSession *cdb.Session, name string) *cdbm.OperatingSystem {
	operatingSystem := &cdbm.OperatingSystem{
		ID:        uuid.New(),
		Name:      name,
		Status:    cdbm.OperatingSystemStatusPending,
		CreatedBy: uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(operatingSystem).Exec(context.Background())
	assert.Nil(t, err)
	return operatingSystem
}

func testAllocationBuildSubnet(t *testing.T, dbSession *cdb.Session, tenant *cdbm.Tenant, vpc *cdbm.Vpc, name string, prefix string, ipb *cdbm.IPBlock) *cdbm.Subnet {
	subnet := &cdbm.Subnet{
		ID:          uuid.New(),
		Name:        name,
		SiteID:      vpc.SiteID,
		VpcID:       vpc.ID,
		TenantID:    tenant.ID,
		Status:      cdbm.SubnetStatusPending,
		IPv4Prefix:  &prefix,
		IPv4BlockID: &ipb.ID,
		CreatedBy:   uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(subnet).Exec(context.Background())
	assert.Nil(t, err)
	return subnet
}

func testAllocationBuildVpcPrefix(t *testing.T, dbSession *cdb.Session, tenant *cdbm.Tenant, vpc *cdbm.Vpc, name string, ipb *cdbm.IPBlock) *cdbm.VpcPrefix {
	vpcPrefix := &cdbm.VpcPrefix{
		ID:           uuid.New(),
		Name:         name,
		SiteID:       vpc.SiteID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		Status:       cdbm.VpcPrefixStatusReady,
		Prefix:       ipb.Prefix,
		PrefixLength: ipb.PrefixLength,
		IPBlockID:    &ipb.ID,
		CreatedBy:    uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(vpcPrefix).Exec(context.Background())
	assert.Nil(t, err)
	return vpcPrefix
}

func testAllocationBuildVpc(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, tenant *cdbm.Tenant, org, name string) *cdbm.Vpc {
	vpc := &cdbm.Vpc{
		ID:                       uuid.New(),
		Name:                     name,
		Org:                      org,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		TenantID:                 tenant.ID,
		Status:                   cdbm.VpcStatusPending,
		CreatedBy:                uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(vpc).Exec(context.Background())
	assert.Nil(t, err)
	return vpc
}

func TestAllocationHandler_Create(t *testing.T) {
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
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2}, tnRoles)

	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	assert.NotNil(t, ip)
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)

	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)
	site2 := testIPBlockBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site2)
	site3 := testIPBlockBuildSite(t, dbSession, ip2, "testSite3", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site3)
	site4 := testIPBlockBuildSite(t, dbSession, ip2, "testSite4", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site4)
	site5 := testIPBlockBuildSite(t, dbSession, ip2, "testSite5", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site5)

	tenant1 := testMachineBuildTenant(t, dbSession, tnOrg1, "t1")
	tenant2 := testMachineBuildTenant(t, dbSession, tnOrg2, "t2")
	tenant3 := testMachineBuildTenant(t, dbSession, tnOrg2, "t3")
	tenant4 := testMachineBuildTenant(t, dbSession, tnOrg2, "t4")

	cfg := common.GetTestConfig()

	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	it1 := common.TestBuildInstanceType(t, dbSession, "testIT", cutil.GetPtr(uuid.New()), site, map[string]string{
		"name":        "test-instance-type-1",
		"description": "Test Instance Type 1 Description",
	}, ipu)

	it2 := common.TestBuildInstanceType(t, dbSession, "testIT2", cutil.GetPtr(uuid.New()), site2, map[string]string{
		"name":        "test-instance-type-2",
		"description": "Test Instance Type 2 Description",
	}, ipu)

	it3 := common.TestBuildInstanceType(t, dbSession, "testIT3", cutil.GetPtr(uuid.New()), site, map[string]string{
		"name":        "test-instance-type-3",
		"description": "Test Instance Type 3 Description",
	}, ipu)

	it4 := common.TestBuildInstanceType(t, dbSession, "testIT4", cutil.GetPtr(uuid.New()), site2, map[string]string{
		"name":        "test-instance-type-4",
		"description": "Test Instance Type 4 Description",
	}, ipu)

	// build some machines, and map the machines to the instancetypes
	for i := 1; i <= 7; i++ {
		mc := testInstanceBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(false), nil)
		assert.NotNil(t, mc)
		mc2 := testInstanceBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(false), nil)
		assert.NotNil(t, mc2)
		mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc, it1)
		assert.NotNil(t, mcinst1)
		mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc2, it3)
		assert.NotNil(t, mcinst2)
	}

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "test1pb", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	ipb2 := testIPBlockBuildIPBlock(t, dbSession, "test2pb", site2, ip2, &tenant2.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	ipb3 := testIPBlockBuildIPBlock(t, dbSession, "test3pb", site3, ip2, &tenant2.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	ipb4 := testIPBlockBuildIPBlock(t, dbSession, "test4pb", site4, ip2, &tenant3.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "196.162.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	ipb5 := testIPBlockBuildIPBlock(t, dbSession, "test4pb", site5, ip2, &tenant4.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "194.162.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)

	// Matching the provider and site is not enough. A record with SitePrefixID
	// set belongs to SitePrefix and cannot be the parent of an Allocation.
	tenantSitePrefix := testIPBlockBuildTenantSitePrefix(t, dbSession, "private-site-prefix", site, ip, tenant1, "192.169.0.0", 16, cdbm.IPBlockStatusReady, ipu)

	ipbFG := testIPBlockBuildIPBlock(t, dbSession, "testipbFG", site, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.170.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)

	acBadInstanceTypeDoesNotExist := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: uuid.New().String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 5}
	acBadInstanceTypeProviderMismatch := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it2.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 5}
	acBadInstanceTypeSiteMismatch := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it4.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 5}
	acGoodIT := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 2}
	acGoodITHigh := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 5}

	acBadIPBlockDoesNotExist := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: uuid.New().String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}
	acBadIPBlockProviderMismatch := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipb2.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}
	acBadIPBlockSiteMismatch := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipb3.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}
	acBadIPBBlockSizeLargerThanParent := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipb1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 15}
	acTenantSitePrefix := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: tenantSitePrefix.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}
	acGoodIPB := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipb1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}

	acGoodIPBFG := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipbFG.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 16}

	okBodyIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	errBodyITMachinesUnavailable, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok2", Description: cutil.GetPtr(""), TenantID: tenant2.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodITHigh}})
	assert.Nil(t, err)
	okBodyITForDifferentTenant, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok3", Description: cutil.GetPtr(""), TenantID: tenant2.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	errBodyIPBNameClash, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPB}})
	assert.Nil(t, err)
	okBodyIPB, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okIpb", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPB}})
	assert.Nil(t, err)

	okBodyIPBFG, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okIpbFG", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPBFG}})
	assert.Nil(t, err)

	errBodyDoesntValidate, err := json.Marshal(struct{ Name string }{Name: "test"})
	assert.Nil(t, err)
	errBodyBadSiteID, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok6", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: uuid.New().String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	errBodyBadSiteIP, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok7", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site2.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	errBodyBadTenantID, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok8", Description: cutil.GetPtr(""), TenantID: uuid.New().String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)

	errBodyBadITInAC, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok9", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acBadInstanceTypeDoesNotExist}})
	assert.Nil(t, err)
	okBodyRepeatedITInAC, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok-repeated", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	errBodyBadIPInIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok10", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acBadInstanceTypeProviderMismatch}})
	assert.Nil(t, err)
	errBodyBadSiteInIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok11", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acBadInstanceTypeSiteMismatch}})
	assert.Nil(t, err)
	errBodyBadIPBInAC, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok12", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acBadIPBlockDoesNotExist}})
	assert.Nil(t, err)
	errBodyTenantSitePrefixInAC, err := json.Marshal(model.APIAllocationCreateRequest{Name: "private-ipblock", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acTenantSitePrefix}})
	assert.Nil(t, err)
	errBodyBadIPInIPB, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok13", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acBadIPBlockProviderMismatch}})
	assert.Nil(t, err)
	errBodyBadSiteInIPB, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok14", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site2.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acBadIPBlockSiteMismatch}})
	assert.Nil(t, err)

	errAllocationConstraintHasBlockSizeLargerThanParent, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok1-ipb-error", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acBadIPBBlockSizeLargerThanParent}})
	assert.Nil(t, err)

	errBodyIpamFail, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok15", Description: cutil.GetPtr(""), TenantID: tenant2.ID.String(), SiteID: site2.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acBadIPBlockProviderMismatch}})
	assert.Nil(t, err)

	parentPref, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref)

	parentPrefFG1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbFG.Prefix, ipbFG.PrefixLength, ipbFG.RoutingType, ipbFG.InfrastructureProviderID.String(), ipbFG.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPrefFG1)

	parentPrefIBP1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb4.Prefix, ipb4.PrefixLength, ipb4.RoutingType, ipb4.InfrastructureProviderID.String(), ipb4.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPrefIBP1)

	parentPrefIBP2, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb5.Prefix, ipb5.PrefixLength, ipb5.RoutingType, ipb5.InfrastructureProviderID.String(), ipb5.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPrefIBP2)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock Temporal call
	tmc1 := &tmocks.Client{}
	wid1 := "test-workflow-id"
	wrun1 := &tmocks.WorkflowRun{}
	wrun1.On("GetID").Return(wid1)

	tmc1.Mock.On("ExecuteWorkflow", mock.Anything,
		mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, *uuid.UUID, *uuid.UUID, *uuid.UUID) error"),
		mock.AnythingOfType("*uuid.UUID"), mock.AnythingOfType("*uuid.UUID"), mock.AnythingOfType("*uuid.UUID")).Return(wrun1, nil)

	// Mock Temporal call
	tmc2 := &tmocks.Client{}
	wid2 := "test-workflow-id"
	wrun2 := &tmocks.WorkflowRun{}
	wrun2.On("GetID").Return(wid2)

	tmc2.Mock.On("ExecuteWorkflow", mock.Anything,
		mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, *uuid.UUID, *uuid.UUID, *uuid.UUID) error"),
		mock.AnythingOfType("*uuid.UUID"), mock.AnythingOfType("*uuid.UUID"), mock.AnythingOfType("*uuid.UUID")).Return(wrun2, fmt.Errorf("Failed to execute workflow"))

	// Mock Temporal Site Client pool
	tsc := &tmocks.Client{}

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site.ID.String()] = tsc
	scp.IDClientMap[site2.ID.String()] = tsc
	scp.IDClientMap[site3.ID.String()] = tsc
	scp.IDClientMap[site4.ID.String()] = tsc
	scp.IDClientMap[site5.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateTenant", mock.Anything).Return(wrun, nil)

	tests := []struct {
		name                 string
		reqOrgName           string
		reqBody              string
		user                 *cdbm.User
		expectedErr          bool
		expectNameErrMsg     string
		expectedStatus       int
		expectedIpam         bool
		expectedIpamErrMsg   string
		expectedInstanceType bool
		expectedIPBlock      bool
		checkFullGrant       bool
		verifyChildSpanner   bool
		tmc                  *tmocks.Client
	}{
		{
			name:           "error when User not found in request context",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBodyIT),
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when User not found in Org",
			reqOrgName:     "SomeOrg",
			reqBody:        string(okBodyIT),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when request body doesn't bind",
			reqOrgName:     ipOrg1,
			reqBody:        "SomeNonJsonBody",
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when request doesn't validate",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyDoesntValidate),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Infrastructure Provider doesnt exist for Org",
			reqOrgName:     ipOrg3,
			reqBody:        string(okBodyIT),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Site in request doesnt exist",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyBadSiteID),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Site specified in request belongs to a different Provider than current Org's",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyBadSiteIP),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Tenant in request doesnt exist",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyBadTenantID),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Instance Type in Allocation Constraint doesn't exist",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyBadITInAC),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Instance Type's Provider does not match with current Org's Provider",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyBadIPInIT),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Instance Type's Site is different from Allocation's Site",
			reqOrgName:     ipOrg2,
			reqBody:        string(errBodyBadSiteInIT),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when User does not have correct role",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBodyIT),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:                 "success case with Allocation Constraint resource type is Instance Type",
			reqOrgName:           ipOrg1,
			reqBody:              string(okBodyIT),
			user:                 ipu,
			expectedErr:          false,
			expectedStatus:       http.StatusCreated,
			expectedInstanceType: true,
			verifyChildSpanner:   true,
		},
		{
			name:           "success when Allocation Constraint already exists for Instance Type for the Tenant",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBodyRepeatedITInAC),
			user:           ipu,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
		},
		{
			name:           "error when Allocation Contraint with Instance Type is not satisfiable by available Machines",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyITMachinesUnavailable),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:           "success when Allocation Contraint with Instance Type exists for another tenant",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBodyITForDifferentTenant),
			user:           ipu,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
		},
		{
			name:           "error when IP Block in Allocation Constraint doesn't exist",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyBadIPBInAC),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "Tenant SitePrefix cannot be used as an Allocation parent",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyTenantSitePrefixInAC),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when IP Block in Allocation Constraint belongs to a different Provider than current Org's",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyBadIPInIPB),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when IP Block in Allocation Constraint belongs to a different Site than specified in request",
			reqOrgName:     ipOrg2,
			reqBody:        string(errBodyBadSiteInIPB),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "error when IPAM allocation for child IP Block fails",
			reqOrgName:         ipOrg2,
			reqBody:            string(errBodyIpamFail),
			user:               ipu,
			expectedErr:        true,
			expectedIpamErrMsg: "Could not create child IPAM entry for Allocation Constraint. Details: unable to find prefix for cidr:192.168.0.0/16",
			expectedStatus:     http.StatusConflict,
		},
		{
			name:           "error when Allocation Constraint value is larger than parent IP Block size",
			reqOrgName:     ipOrg1,
			reqBody:        string(errAllocationConstraintHasBlockSizeLargerThanParent),
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:             "error when Allocation with same name already exists",
			reqOrgName:       ipOrg1,
			reqBody:          string(errBodyIPBNameClash),
			user:             ipu,
			expectedErr:      true,
			expectNameErrMsg: "id",
			expectedStatus:   http.StatusConflict,
		},
		{
			name:            "success when Allocation Constraint with IP Block is specified",
			reqOrgName:      ipOrg1,
			reqBody:         string(okBodyIPB),
			user:            ipu,
			expectedErr:     false,
			expectedStatus:  http.StatusCreated,
			expectedIpam:    true,
			expectedIPBlock: true,
		},
		{
			name:           "success when Allocation Constraint with Full Grant",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBodyIPBFG),
			user:           ipu,
			expectedErr:    false,
			expectedStatus: http.StatusCreated,
			expectedIpam:   false,
			checkFullGrant: true,
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

			cipbh := CreateAllocationHandler{
				dbSession: dbSession,
				tc:        tc.tmc,
				scp:       scp,
				cfg:       cfg,
			}
			err := cipbh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusCreated)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APIAllocation{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, len(rsp.StatusHistory), 1)
				// validate allocation constraint record exists
				assert.Equal(t, 1, len(rsp.AllocationConstraints))

				if tc.expectedInstanceType || tc.expectedIpam {
					childIT := rsp.AllocationConstraints[0].ID
					childITUUID, err := uuid.Parse(childIT)
					assert.Nil(t, err)
					acDAO := cdbm.NewAllocationConstraintDAO(dbSession)
					_, err = acDAO.GetByID(ctx, nil, childITUUID, nil)
					assert.Nil(t, err)

					if tc.expectedInstanceType {
						assert.NotNil(t, rsp.AllocationConstraints[0].InstanceType)
						assert.Nil(t, rsp.AllocationConstraints[0].IPBlock)
					}

				}
				// validate ipam exists for allocation constraint if ipblock
				if tc.expectedIpam {
					childIPBID := rsp.AllocationConstraints[0].DerivedResourceID
					childIPBUUID, err := uuid.Parse(*childIPBID)
					assert.Nil(t, err)
					ipbDAO := cdbm.NewIPBlockDAO(dbSession)
					childIPB, err := ipbDAO.GetByID(ctx, nil, childIPBUUID, nil)
					assert.Nil(t, err)

					sdDAO := cdbm.NewStatusDetailDAO(dbSession)
					_, sdcount, err := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{childIPBUUID.String()}}, cdbp.PageInput{})
					assert.NoError(t, err)
					assert.Equal(t, sdcount, 1)

					ipamer := cipam.NewWithStorage(ipamStorage)
					ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(ctx, childIPB.RoutingType, childIPB.InfrastructureProviderID.String(), childIPB.SiteID.String()))
					pref := ipamer.PrefixFrom(ctx, ipam.GetCidrForIPBlock(ctx, childIPB.Prefix, childIPB.PrefixLength))
					assert.NotNil(t, pref)

					if tc.expectedIPBlock {
						assert.Nil(t, rsp.AllocationConstraints[0].InstanceType)
						assert.NotNil(t, rsp.AllocationConstraints[0].IPBlock)
					}

				}
				if tc.checkFullGrant {
					parentIPBID := rsp.AllocationConstraints[0].ResourceTypeID
					parentIPBUUID, err := uuid.Parse(parentIPBID)
					assert.Nil(t, err)
					ipbDAO := cdbm.NewIPBlockDAO(dbSession)
					parentIPB, err := ipbDAO.GetByID(ctx, nil, parentIPBUUID, nil)
					assert.NoError(t, err)
					assert.Equal(t, true, parentIPB.FullGrant)
				}

				// Check Tenant/Site association
				tsDAO := cdbm.NewTenantSiteDAO(dbSession)
				tenantID := uuid.MustParse(rsp.TenantID)
				siteID := uuid.MustParse(rsp.SiteID)
				_, tscount, err := tsDAO.GetAll(
					ctx,
					nil,
					cdbm.TenantSiteFilterInput{
						TenantIDs: []uuid.UUID{tenantID},
						SiteIDs:   []uuid.UUID{siteID},
					},
					cdbp.PageInput{},
					nil,
				)
				assert.Nil(t, err)
				assert.Equal(t, 1, tscount)
			} else {
				if tc.expectedIpamErrMsg != "" {
					assert.Contains(t, rec.Body.String(), tc.expectedIpamErrMsg)
				}
				if tc.expectNameErrMsg != "" {
					assert.Contains(t, rec.Body.String(), tc.expectNameErrMsg)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}

	t.Run("reports an allocation parent deleted before its row lock", func(t *testing.T) {
		raceName := "allocation-delete-first-" + uuid.NewString()
		raceRoot := testIPBlockBuildIPBlock(
			t,
			dbSession,
			raceName,
			site,
			ip,
			nil,
			cdbm.IPBlockRoutingTypeDatacenterOnly,
			"198.18.0.0",
			24,
			cdbm.IPBlockProtocolVersionV4,
			false,
			cdbm.IPBlockStatusReady,
			ipu,
		)
		raceRootPrefix, err := ipam.CreateIpamEntryForIPBlock(
			ctx,
			ipamStorage,
			raceRoot.Prefix,
			raceRoot.PrefixLength,
			raceRoot.RoutingType,
			raceRoot.InfrastructureProviderID.String(),
			raceRoot.SiteID.String(),
		)
		require.NoError(t, err)
		require.NotNil(t, raceRootPrefix)

		raceCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
		deleteTx, err := cdb.BeginTx(raceCtx, dbSession, nil)
		require.NoError(t, err)
		deleteFinished := false
		handlerStarted := false
		handlerDrained := false
		handlerDone := make(chan error, 1)
		defer func() {
			if !deleteFinished {
				_ = deleteTx.Rollback()
			}
			cancel()
			if handlerStarted && !handlerDrained {
				select {
				case <-handlerDone:
				case <-time.After(5 * time.Second):
					t.Error("Allocation handler did not stop during cleanup")
				}
			}
		}()

		var deleteBackendPID int
		err = deleteTx.GetBunTx().NewSelect().
			ColumnExpr("pg_backend_pid()").
			Scan(raceCtx, &deleteBackendPID)
		require.NoError(t, err)

		ipbDAO := cdbm.NewIPBlockDAO(dbSession)
		lockedRoot, err := ipbDAO.GetByIDForUpdate(raceCtx, deleteTx, raceRoot.ID)
		require.NoError(t, err)
		require.NoError(t, ipbDAO.Delete(raceCtx, deleteTx, lockedRoot.ID))
		deleteStorage := ipam.NewIpamStorage(dbSession.DB, deleteTx.GetBunTx())
		require.NoError(t, ipam.DeleteIpamEntryForIPBlock(
			raceCtx,
			deleteStorage,
			lockedRoot.Prefix,
			lockedRoot.PrefixLength,
			lockedRoot.RoutingType,
			lockedRoot.InfrastructureProviderID.String(),
			lockedRoot.SiteID.String(),
		))

		constraint := model.APIAllocationConstraintCreateRequest{
			ResourceType:    cdbm.AllocationResourceTypeIPBlock,
			ResourceTypeID:  raceRoot.ID.String(),
			ConstraintType:  cdbm.AllocationConstraintTypeReserved,
			ConstraintValue: 28,
		}
		body, err := json.Marshal(model.APIAllocationCreateRequest{
			Name:                  raceName,
			Description:           cutil.GetPtr(""),
			TenantID:              tenant1.ID.String(),
			SiteID:                site.ID.String(),
			AllocationConstraints: []model.APIAllocationConstraintCreateRequest{constraint},
		})
		require.NoError(t, err)

		e := echo.New()
		req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(body)))
		req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		rec := httptest.NewRecorder()
		ec := e.NewContext(req, rec)
		ec.SetParamNames("orgName")
		ec.SetParamValues(ipOrg1)
		ec.Set("user", ipu)
		requestCtx := context.WithValue(raceCtx, otelecho.TracerKey, tracer) //nolint:staticcheck // Middleware owns the context key.
		ec.SetRequest(ec.Request().WithContext(requestCtx))

		handlerStarted = true
		go func() {
			handlerDone <- (CreateAllocationHandler{
				dbSession: dbSession,
				scp:       scp,
				cfg:       cfg,
			}).Handle(ec)
		}()

		require.Eventually(t, func() bool {
			var blockedHandlers int
			queryErr := dbSession.DB.NewSelect().
				ColumnExpr("count(*)").
				TableExpr("pg_catalog.pg_stat_activity").
				Where("datname = current_database()").
				Where("state = 'active'").
				Where("wait_event_type = 'Lock'").
				Where("query ILIKE ?", "%ip_block%").
				Where("query ILIKE ?", "%FOR UPDATE%").
				Where("? = ANY(pg_blocking_pids(pid))", deleteBackendPID).
				Scan(raceCtx, &blockedHandlers)
			return queryErr == nil && blockedHandlers > 0
		}, 5*time.Second, 10*time.Millisecond, "Allocation creation did not wait for the deleting transaction's IP Block row lock")

		err = deleteTx.Commit()
		if err == nil {
			deleteFinished = true
		}
		require.NoError(t, err)
		select {
		case err = <-handlerDone:
			handlerDrained = true
			require.NoError(t, err)
		case <-raceCtx.Done():
			t.Fatalf("Allocation creation did not resume after deletion committed: %v", raceCtx.Err())
		}
		assert.Equal(t, http.StatusBadRequest, rec.Code)
		assert.Contains(t, rec.Body.String(), "The IP Block in the Allocation Constraint no longer exists")

		allocationCount, err := cdbm.NewAllocationDAO(dbSession).GetCount(
			ctx,
			nil,
			cdbm.AllocationFilterInput{
				Name:      &raceName,
				TenantIDs: []uuid.UUID{tenant1.ID},
				SiteIDs:   []uuid.UUID{site.ID},
			},
		)
		require.NoError(t, err)
		assert.Zero(t, allocationCount)

		constraintCount, err := dbSession.DB.NewSelect().
			Model((*cdbm.AllocationConstraint)(nil)).
			Where("ac.resource_type = ?", cdbm.AllocationResourceTypeIPBlock).
			Where("ac.resource_type_id = ?", raceRoot.ID).
			Count(ctx)
		require.NoError(t, err)
		assert.Zero(t, constraintCount)

		childCount, err := dbSession.DB.NewSelect().
			Model((*cdbm.IPBlock)(nil)).
			Where("ipb.name = ?", raceName).
			Where("ipb.tenant_id = ?", tenant1.ID).
			Where("ipb.site_id = ?", site.ID).
			Count(ctx)
		require.NoError(t, err)
		assert.Zero(t, childCount)

		_, err = ipbDAO.GetByID(ctx, nil, raceRoot.ID, nil)
		require.ErrorIs(t, err, cdb.ErrDoesNotExist)
		ipamer := cipam.NewWithStorage(ipamStorage)
		ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
			ctx,
			raceRoot.RoutingType,
			raceRoot.InfrastructureProviderID.String(),
			raceRoot.SiteID.String(),
		))
		assert.Nil(t, ipamer.PrefixFrom(ctx, raceRootPrefix.Cidr))
	})
}

func testCreateAllocation(t *testing.T, dbSession *cdb.Session, ipamStorage cipam.Storage, user *cdbm.User, reqOrgName, reqBody string) *model.APIAllocation {
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

	// Mock Temporal Site Client pool for ALlocaiton creation
	tsc := &tmocks.Client{}

	cfg := common.GetTestConfig()

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	acr := model.APIAllocationCreateRequest{}
	err := json.Unmarshal([]byte(reqBody), &acr)
	assert.NoError(t, err)

	scp.IDClientMap[acr.SiteID] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateTenant", mock.Anything).Return(wrun, nil)

	cipbh := CreateAllocationHandler{
		dbSession: dbSession,
		tc:        &tmocks.Client{},
		scp:       scp,
		cfg:       cfg,
	}
	err = cipbh.Handle(ec)
	assert.Nil(t, err)
	assert.Equal(t, http.StatusCreated, rec.Code)
	rsp := &model.APIAllocation{}
	err = json.Unmarshal(rec.Body.Bytes(), rsp)
	assert.Nil(t, err)
	return rsp
}

func TestAllocationHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}
	ipViewerRoles := []string{authz.ProviderViewerRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)
	ipuv := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipViewerRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2}, tnRoles)

	nru := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1}, []string{})

	ip := common.TestBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	ip2 := common.TestBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)
	assert.NotNil(t, ip)

	site1 := testIPBlockBuildSite(t, dbSession, ip, "testSite-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site1)
	site2 := testIPBlockBuildSite(t, dbSession, ip, "testSite-2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site2)

	tenant1 := common.TestBuildTenantWithDisplayName(t, dbSession, "test-tenant-1", tnOrg1, tnu, "Test Tenant 1")
	assert.NotNil(t, tenant1)
	tenant2 := common.TestBuildTenantWithDisplayName(t, dbSession, "test-tenant-2", tnOrg1, tnu, "Test Tenant 2")
	assert.NotNil(t, tenant2)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site1, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)

	parentPref, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref)

	acGoodIPB := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipb1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}

	totalCount := 30

	var al *model.APIAllocation
	almap := map[string]*model.APIAllocation{}

	var resourceTypeIDs []string
	for i := 0; i < totalCount; i++ {
		if i%2 == 0 {
			itTmp := testMachineBuildInstanceType(t, dbSession, ip, site1, "testIT")
			resourceTypeIDs = append(resourceTypeIDs, itTmp.ID.String())
			// build some machines, and map the machines to the instancetypes
			for j := 1; j <= 2; j++ {
				mc := testInstanceBuildMachine(t, dbSession, ip.ID, site1.ID, cutil.GetPtr(false), nil)
				assert.NotNil(t, mc)
				mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc, itTmp)
				assert.NotNil(t, mcinst1)
			}
			acGoodIT := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: itTmp.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 1}
			okBodyIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: fmt.Sprintf("allocation-%02d", i), Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site1.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
			assert.Nil(t, err)

			al = testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIT))
			assert.NotNil(t, al)
		} else {
			okBodyIPB, err := json.Marshal(model.APIAllocationCreateRequest{Name: fmt.Sprintf("allocation-%02d", i), Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site1.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPB}})
			assert.Nil(t, err)

			al = testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIPB))
			assert.NotNil(t, al)
		}

		alID := uuid.MustParse(al.ID)
		almap[al.Name] = al
		common.TestBuildStatusDetail(t, dbSession, alID.String(), cdbm.AllocationStatusRegistered, cutil.GetPtr("Allocation is now registered"))
	}

	// Org with both Provider and Tenant roles: same org acts as its own infrastructure provider and tenant
	orgName := "test-provider-and-tenant-org"
	orgRoles := []string{authz.ProviderAdminRole, authz.TenantAdminRole}
	orgUser := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{orgName}, orgRoles)
	orgProvider := common.TestBuildInfrastructureProvider(t, dbSession, "test-org-provider", orgName, orgUser)
	orgSite := testIPBlockBuildSite(t, dbSession, orgProvider, "test-org-site", cdbm.SiteStatusRegistered, true, orgUser)
	orgTenant := common.TestBuildTenantWithDisplayName(t, dbSession, "test-org-tenant", orgName, orgUser, "Test Org Tenant")

	orgAllocCount := 5
	for i := 0; i < orgAllocCount; i++ {
		itTmp := testMachineBuildInstanceType(t, dbSession, orgProvider, orgSite, fmt.Sprintf("test-org-instance-type-%02d", i))
		for j := 1; j <= 2; j++ {
			mc := testInstanceBuildMachine(t, dbSession, orgProvider.ID, orgSite.ID, cutil.GetPtr(false), nil)
			mcinst := testInstanceBuildMachineInstanceType(t, dbSession, mc, itTmp)
			assert.NotNil(t, mcinst)
		}
		acOrg := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: itTmp.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 1}
		orgBody, err := json.Marshal(model.APIAllocationCreateRequest{Name: fmt.Sprintf("org-allocation-%02d", i), Description: cutil.GetPtr(""), TenantID: orgTenant.ID.String(), SiteID: orgSite.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acOrg}})
		assert.Nil(t, err)
		orgAl := testCreateAllocation(t, dbSession, ipamStorage, orgUser, orgName, string(orgBody))
		assert.NotNil(t, orgAl)
		common.TestBuildStatusDetail(t, dbSession, orgAl.ID, cdbm.AllocationStatusRegistered, cutil.GetPtr("Allocation is now registered"))
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		queryInfrastructureProviderID     *string
		queryTenantIDs                    []string
		querySiteIDs                      []string
		querySearch                       *string
		queryStatuses                     []string
		queryResourceTypes                []string
		queryResourceTypeIDs              []string
		queryConstraintTypes              []string
		queryConstraintValues             []string
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		queryIncludeRelations3            *string
		pageNumber                        *int
		pageSize                          *int
		orderBy                           *string
		expectedErr                       bool
		expectedStatus                    int
		expectedCnt                       int
		expectedTotal                     *int
		expectedFirstEntry                *cdbm.Allocation
		expectedTenantOrg                 *string
		expectedInfrastructureProviderOrg *string
		expectedSiteName                  *string
		verifyChildSpanner                bool
		thttp                             *httptest.Server
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "success when no provider or tenant query params specified (inferred from org)",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySiteIDs:   []string{site1.ID.String()},
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			// Org has both Provider and Tenant roles. Every allocation has
			// InfrastructureProviderID = orgProvider and TenantID = orgTenant, so it appears in both
			// the provider and tenant DB queries. The mapset must deduplicate them so the response
			// contains exactly orgAllocCount unique allocations, not 2×orgAllocCount.
			name:           "success when org has both provider and tenant roles (results deduplicated)",
			reqOrgName:     orgName,
			user:           orgUser,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    orgAllocCount,
			expectedTotal:  &orgAllocCount,
		},
		{
			name:           "error when site id not valid uuid",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryTenantIDs: []string{tenant1.ID.String()},
			querySiteIDs:   []string{"non-uuid"},
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when site id not found",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryTenantIDs: []string{tenant1.ID.String()},
			querySiteIDs:   []string{uuid.New().String()},
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:                          "error when user does not have the necessary role",
			reqOrgName:                    ipOrg1,
			user:                          nru,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			expectedErr:                   true,
			expectedStatus:                http.StatusForbidden,
		},
		{
			name:                          "success when infrastructure provider id is specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			queryIncludeRelations1:        nil,
			queryIncludeRelations2:        nil,
			queryIncludeRelations3:        nil,
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   paginator.DefaultLimit,
			expectedTotal:                 &totalCount,
		},
		{
			name:                          "success case when user has Provider viewer role",
			reqOrgName:                    ipOrg1,
			user:                          ipuv,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			queryIncludeRelations1:        nil,
			queryIncludeRelations2:        nil,
			queryIncludeRelations3:        nil,
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   paginator.DefaultLimit,
			expectedTotal:                 &totalCount,
		},
		{
			name:           "success when tenant id is specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryTenantIDs: []string{tenant1.ID.String()},
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when tenant id and site id is specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryTenantIDs: []string{tenant1.ID.String()},
			querySiteIDs:   []string{site1.ID.String()},
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:                          "success when both infrastructure id and tenant id are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			queryTenantIDs:                []string{tenant1.ID.String()},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   paginator.DefaultLimit,
			expectedTotal:                 &totalCount,
		},
		{
			name:                          "success when infrastructure id and site id are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   paginator.DefaultLimit,
			expectedTotal:                 &totalCount,
		},
		{
			name:                          "success when infrastructure id, tenant id and site id are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			queryTenantIDs:                []string{tenant1.ID.String()},
			querySiteIDs:                  []string{site1.ID.String()},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   paginator.DefaultLimit,
			expectedTotal:                 &totalCount,
		},
		{
			name:                              "success when infrastructure provider relation is specified",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			queryInfrastructureProviderID:     cutil.GetPtr(ip.ID.String()),
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2:            nil,
			queryIncludeRelations3:            nil,
			expectedErr:                       false,
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
			expectedStatus:                    http.StatusOK,
			expectedCnt:                       paginator.DefaultLimit,
			expectedTotal:                     &totalCount,
		},
		{
			name:                              "success when infrastructure and tenant relations are specified",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			queryInfrastructureProviderID:     cutil.GetPtr(ip.ID.String()),
			queryTenantIDs:                    []string{tenant1.ID.String()},
			querySiteIDs:                      []string{site1.ID.String()},
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2:            cutil.GetPtr(cdbm.TenantRelationName),
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
			expectedTenantOrg:                 cutil.GetPtr(tenant1.Org),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedCnt:                       paginator.DefaultLimit,
			expectedTotal:                     &totalCount,
		},
		{
			name:                              "success when infrastructure and site relations are specified",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			queryInfrastructureProviderID:     cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                      []string{site1.ID.String()},
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations3:            cutil.GetPtr(cdbm.SiteRelationName),
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
			expectedSiteName:                  cutil.GetPtr(site1.Name),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedCnt:                       paginator.DefaultLimit,
			expectedTotal:                     &totalCount,
		},
		{
			name:                          "success when no results are returned",
			reqOrgName:                    ipOrg2,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip2.ID.String()),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   0,
			verifyChildSpanner:            true,
		},
		{
			name:                          "success when pagination params are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("NAME_DESC"),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   10,
			expectedTotal:                 &totalCount,
			expectedFirstEntry:            &cdbm.Allocation{Name: "allocation-29"},
		},
		{
			name:                          "failure when invalid pagination params are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("TEST_ASC"),
			expectedErr:                   true,
			expectedStatus:                http.StatusBadRequest,
			expectedCnt:                   0,
		},
		{
			name:                          "success when name search query is specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			querySearch:                   cutil.GetPtr("allocation-"),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   paginator.DefaultLimit,
			expectedTotal:                 &totalCount,
		},
		{
			name:                          "success when unexisted name search query are specified return none",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			querySearch:                   cutil.GetPtr("test-"),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   0,
			expectedTotal:                 cutil.GetPtr(0),
		},
		{
			name:                          "success when combination name and status search query are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			querySearch:                   cutil.GetPtr("allocation Registered"),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   paginator.DefaultLimit,
			expectedTotal:                 &totalCount,
		},
		{
			name:                          "success when instance type as a resource type search query are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryResourceTypes:            []string{cdbm.AllocationResourceTypeInstanceType},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   15,
			expectedTotal:                 cutil.GetPtr(15),
		},
		{
			name:                          "success when ipblock as a resource type search query are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryResourceTypes:            []string{cdbm.AllocationResourceTypeIPBlock},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   15,
			expectedTotal:                 cutil.GetPtr(15),
		},
		{
			name:                          "error when given resource type search query are specified not exists",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryResourceTypes:            []string{"NotExisted"},
			expectedErr:                   true,
			expectedStatus:                http.StatusBadRequest,
			expectedCnt:                   0,
			expectedTotal:                 cutil.GetPtr(0),
		},
		{
			name:                          "success when AllocationStatusRegistered status is specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryStatuses:                 []string{cdbm.AllocationStatusRegistered},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   paginator.DefaultLimit,
			expectedTotal:                 &totalCount,
		},
		{
			name:                          "error when BadStatus status is specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryStatuses:                 []string{"BadStatus"},
			expectedErr:                   true,
			expectedStatus:                http.StatusBadRequest,
			expectedCnt:                   0,
			expectedTotal:                 cutil.GetPtr(0),
		},
		{
			name:                          "success when multiple resource types are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryResourceTypes:            []string{cdbm.AllocationResourceTypeIPBlock, cdbm.AllocationResourceTypeInstanceType},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   20,
			expectedTotal:                 cutil.GetPtr(30),
		},
		{
			name:                          "success when single resource type id is specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryResourceTypeIDs:          []string{resourceTypeIDs[0]},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   1,
			expectedTotal:                 cutil.GetPtr(1),
		},
		{
			name:                          "success when multiple resource type ids are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryResourceTypeIDs:          resourceTypeIDs[0:2],
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   2,
			expectedTotal:                 cutil.GetPtr(2),
		},
		{
			name:                          "success when single constraint type is specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryConstraintTypes:          []string{cdbm.AllocationConstraintTypeReserved},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   20,
			expectedTotal:                 cutil.GetPtr(30),
		},
		{
			name:                          "success when multiple constraint types are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryConstraintTypes:          []string{cdbm.AllocationConstraintTypeReserved, cdbm.AllocationConstraintTypeOnDemand},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   20,
			expectedTotal:                 cutil.GetPtr(30),
		},
		{
			name:                          "success when single constraint value is specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryConstraintValues:         []string{"1"},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   15,
			expectedTotal:                 cutil.GetPtr(15),
		},
		{
			name:                          "success when multiple constraint values are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryConstraintValues:         []string{"1", "0"},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   15,
			expectedTotal:                 cutil.GetPtr(15),
		},
		{
			name:           "success when multiple tenant ids are specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryTenantIDs: []string{tenant1.ID.String(), tenant2.ID.String()},
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when multiple site ids are specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryTenantIDs: []string{tenant1.ID.String()},
			querySiteIDs:   []string{site1.ID.String(), site2.ID.String()},
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:                          "success when multiple statuses are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			queryStatuses:                 []string{cdbm.AllocationStatusRegistered, cdbm.AllocationStatusPending},
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   paginator.DefaultLimit,
			expectedTotal:                 &totalCount,
		},
		{
			name:                          "success when sort by site name",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("SITE_NAME_ASC"),
			queryIncludeRelations1:        cutil.GetPtr(cdbm.SiteRelationName),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   10,
			expectedTotal:                 &totalCount,
			expectedFirstEntry:            &cdbm.Allocation{Name: "allocation-00"},
		},
		{
			name:                          "success when sort by org display name",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("TENANT_ORG_DISPLAY_NAME_ASC"),
			queryIncludeRelations1:        cutil.GetPtr(cdbm.TenantRelationName),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   10,
			expectedTotal:                 &totalCount,
			expectedFirstEntry:            &cdbm.Allocation{Name: "allocation-00"},
		},
		{
			name:                          "success when sort by instance type name",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("INSTANCE_TYPE_NAME_ASC"),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   10,
			expectedTotal:                 &totalCount,
			expectedFirstEntry:            &cdbm.Allocation{Name: "allocation-00"},
		},
		{
			name:                          "success when sort by ip block name",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("IP_BLOCK_NAME_ASC"),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   10,
			expectedTotal:                 &totalCount,
			expectedFirstEntry:            &cdbm.Allocation{Name: "allocation-01"},
		},
		{
			name:                          "success when sort by constraint value",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			querySiteIDs:                  []string{site1.ID.String()},
			pageNumber:                    cutil.GetPtr(1),
			pageSize:                      cutil.GetPtr(10),
			orderBy:                       cutil.GetPtr("CONSTRAINT_VALUE_ASC"),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			expectedCnt:                   10,
			expectedTotal:                 &totalCount,
			expectedFirstEntry:            &cdbm.Allocation{Name: "allocation-00"},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.queryInfrastructureProviderID != nil {
				q.Set("infrastructureProviderId", *tc.queryInfrastructureProviderID)
			}
			for _, id := range tc.queryTenantIDs {
				q.Add("tenantId", id)
			}
			for _, id := range tc.querySiteIDs {
				q.Add("siteId", id)
			}
			if tc.querySearch != nil {
				q.Set("query", *tc.querySearch)
			}
			for _, status := range tc.queryStatuses {
				q.Add("status", status)
			}
			for _, rType := range tc.queryResourceTypes {
				q.Add("resourceType", rType)
			}
			for _, rTypeId := range tc.queryResourceTypeIDs {
				q.Add("resourceTypeId", rTypeId)
			}
			for _, cType := range tc.queryConstraintTypes {
				q.Add("constraintType", cType)
			}
			for _, cValue := range tc.queryConstraintValues {
				q.Add("constraintValue", cValue)
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
			if tc.pageNumber != nil {
				q.Set("pageNumber", fmt.Sprintf("%v", *tc.pageNumber))
			}
			if tc.pageSize != nil {
				q.Set("pageSize", fmt.Sprintf("%v", *tc.pageSize))
			}
			if tc.orderBy != nil {
				q.Set("orderBy", *tc.orderBy)
			}

			path := fmt.Sprintf("/v2/org/%s/nico/allocation?%s", tc.reqOrgName, q.Encode())

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

			gaah := GetAllAllocationHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}

			err := gaah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			if tc.expectedErr {
				return
			}

			if rec.Code != tc.expectedStatus {
				t.Errorf("response %v", rec.Body.String())
			}
			require.Equal(t, tc.expectedStatus, rec.Code)

			resp := []model.APIAllocation{}
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
				assert.Equal(t, tc.expectedFirstEntry.Name, resp[0].Name)
			}

			if len(resp) > 0 && len(resp[0].AllocationConstraints) > 0 {
				if resp[0].AllocationConstraints[0].ResourceType == cdbm.AllocationResourceTypeInstanceType {
					assert.NotNil(t, resp[0].AllocationConstraints[0].InstanceType)
				}
				if resp[0].AllocationConstraints[0].ResourceType == cdbm.AllocationResourceTypeIPBlock {
					assert.NotNil(t, resp[0].AllocationConstraints[0].IPBlock)
				}
			}

			if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil || tc.queryIncludeRelations3 != nil {
				if tc.expectedInfrastructureProviderOrg != nil {
					assert.Equal(t, *tc.expectedInfrastructureProviderOrg, resp[0].InfrastructureProvider.Org)
				}

				if tc.expectedTenantOrg != nil {
					assert.Equal(t, *tc.expectedTenantOrg, resp[0].Tenant.Org)
				}

				if tc.expectedSiteName != nil {
					assert.Equal(t, *tc.expectedSiteName, resp[0].Site.Name)
				}
			} else {
				if len(resp) > 0 {
					assert.Nil(t, resp[0].Tenant)
					assert.Nil(t, resp[0].InfrastructureProvider)
					assert.Nil(t, resp[0].Site)
				}

				for _, apiAl := range resp {
					assert.Equal(t, 2, len(apiAl.StatusHistory))
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestAllocationHandler_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}
	ipViewerRoles := []string{authz.ProviderViewerRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)
	ipuv := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, ipViewerRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2}, tnRoles)

	nru := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1}, []string{})

	ip := common.TestBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	ip2 := common.TestBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)
	assert.NotNil(t, ip)

	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)

	tenant1 := common.TestBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnu)
	_ = common.TestBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, tnu)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	it1 := testMachineBuildInstanceType(t, dbSession, ip, site, "testIT")
	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)

	// build some machines, and map the machines to the instancetypes
	for i := 1; i <= 5; i++ {
		mc := testInstanceBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(false), nil)
		assert.NotNil(t, mc)
		mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc, it1)
		assert.NotNil(t, mcinst1)
	}

	acGoodIT := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 5}
	acGoodIPB := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipb1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}

	okBodyIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okit", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	okBodyIPB, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okipb", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPB}})
	assert.Nil(t, err)

	parentPref, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref)

	aIT := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIT))
	aIPB := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIPB))
	assert.NotNil(t, aIPB)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		aID                               string
		queryInfrastructureProviderID     *string
		queryTenantID                     *string
		expectedErr                       bool
		expectedStatus                    int
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		queryIncludeRelations3            *string
		expectedTenantOrg                 *string
		expectedInfrastructureProviderOrg *string
		expectedSiteName                  *string
		verifyChildSpanner                bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           ipu,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when allocation id is invalid uuid",
			reqOrgName:     tnOrg1,
			user:           tnu,
			aID:            "bad#uuid$str",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when allocation id not found",
			reqOrgName:     tnOrg1,
			user:           tnu,
			aID:            uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when provider in org doesnt match allocation",
			reqOrgName:     ipOrg2,
			user:           ipu,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when user does not have valid role",
			reqOrgName:     ipOrg1,
			user:           nru,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:                          "success when infrastructure provider id is specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			aID:                           aIT.ID,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
		},
		{
			name:           "error when allocation id is not found",
			reqOrgName:     tnOrg1,
			user:           tnu,
			aID:            uuid.New().String(),
			queryTenantID:  cutil.GetPtr(tenant1.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "success when tenant id is specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			aID:            aIT.ID,
			queryTenantID:  cutil.GetPtr(tenant1.ID.String()),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
		},
		{
			name:                          "success when both infrastructure id and tenant id are specified",
			reqOrgName:                    ipOrg1,
			user:                          ipu,
			aID:                           aIT.ID,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			queryTenantID:                 cutil.GetPtr(tenant1.ID.String()),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			verifyChildSpanner:            true,
		},
		{
			name:                          "success when user has Provider Viewer role",
			reqOrgName:                    ipOrg1,
			user:                          ipuv,
			aID:                           aIT.ID,
			queryInfrastructureProviderID: cutil.GetPtr(ip.ID.String()),
			expectedErr:                   false,
			expectedStatus:                http.StatusOK,
			verifyChildSpanner:            true,
		},
		{
			name:                              "success when both infrastructure and tenant relations are specified",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			aID:                               aIT.ID,
			queryInfrastructureProviderID:     cutil.GetPtr(ip.ID.String()),
			queryTenantID:                     cutil.GetPtr(tenant1.ID.String()),
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2:            cutil.GetPtr(cdbm.TenantRelationName),
			queryIncludeRelations3:            nil,
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
			expectedTenantOrg:                 cutil.GetPtr(tenant1.Org),
		},
		{
			name:                              "success when infrastructure tenant and site relations are specified",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			aID:                               aIT.ID,
			queryInfrastructureProviderID:     cutil.GetPtr(ip.ID.String()),
			queryTenantID:                     cutil.GetPtr(tenant1.ID.String()),
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2:            cutil.GetPtr(cdbm.TenantRelationName),
			queryIncludeRelations3:            cutil.GetPtr(cdbm.SiteRelationName),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
			expectedTenantOrg:                 cutil.GetPtr(tenant1.Org),
			expectedSiteName:                  cutil.GetPtr(site.Name),
		},
		{
			name:                              "success when infrastructure site relations are specified",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			aID:                               aIT.ID,
			queryInfrastructureProviderID:     cutil.GetPtr(ip.ID.String()),
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations3:            cutil.GetPtr(cdbm.SiteRelationName),
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
			expectedSiteName:                  cutil.GetPtr(site.Name),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			q := req.URL.Query()
			if tc.queryInfrastructureProviderID != nil {
				q.Add("infrastructureProviderId", *tc.queryInfrastructureProviderID)
			}
			if tc.queryTenantID != nil {
				q.Add("tenantId", *tc.queryTenantID)
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

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req.URL.RawQuery = q.Encode()
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.aID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := GetAllocationHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APIAllocation{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				assert.Equal(t, 1, len(rsp.AllocationConstraints))
				assert.Equal(t, 1, len(rsp.StatusHistory))

				if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil || tc.queryIncludeRelations3 != nil {
					if tc.expectedInfrastructureProviderOrg != nil {
						assert.Equal(t, *tc.expectedInfrastructureProviderOrg, rsp.InfrastructureProvider.Org)
					}

					if tc.expectedTenantOrg != nil {
						assert.Equal(t, *tc.expectedTenantOrg, rsp.Tenant.Org)
					}
					if tc.expectedSiteName != nil {
						assert.Equal(t, *tc.expectedSiteName, rsp.Site.Name)
					}
				} else {
					assert.Nil(t, rsp.Tenant)
					assert.Nil(t, rsp.InfrastructureProvider)
					assert.Nil(t, rsp.Site)
				}

				if len(rsp.AllocationConstraints) > 0 {
					if rsp.AllocationConstraints[0].ResourceType == cdbm.AllocationResourceTypeInstanceType {
						assert.NotNil(t, rsp.AllocationConstraints[0].InstanceType)
					}
					if rsp.AllocationConstraints[0].ResourceType == cdbm.AllocationResourceTypeIPBlock {
						assert.NotNil(t, rsp.AllocationConstraints[0].IPBlock)
					}
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestAllocationHandler_Update(t *testing.T) {
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
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2}, tnRoles)

	ip := common.TestBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	ip2 := common.TestBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)
	assert.NotNil(t, ip)
	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)

	tenant1 := common.TestBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnu)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	it1 := testMachineBuildInstanceType(t, dbSession, ip, site, "testIT")

	it2 := testMachineBuildInstanceType(t, dbSession, ip, site, "testIT2")

	// build some machines, and map the machines to the instancetypes
	for i := 1; i <= 4; i++ {
		mc := testInstanceBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(false), nil)
		assert.NotNil(t, mc)
		mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc, it1)
		assert.NotNil(t, mcinst1)
		mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc, it2)
		assert.NotNil(t, mcinst2)
	}

	// Build test IP Block
	ipb := common.TestBuildIPBlock(t, dbSession, "test-ip-block", site, &tenant1.ID, cdbm.IPBlockRoutingTypePublic, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, ipu)

	parentPref, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb.Prefix, ipb.PrefixLength, ipb.RoutingType, ipb.InfrastructureProviderID.String(), ipb.SiteID.String())
	assert.NotNil(t, parentPref)

	acGoodIT := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 2}
	acGoodIT2 := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it2.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 2}
	acGoodIPB1 := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipb.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 28}

	okBodyIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: "test-allocation", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	okBodyIT2, err := json.Marshal(model.APIAllocationCreateRequest{Name: "test-allocation-2", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT2}})
	assert.Nil(t, err)
	okBodyIPB, err := json.Marshal(model.APIAllocationCreateRequest{Name: "test-allocation-3", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPB1}})
	assert.Nil(t, err)

	aIT := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIT))
	aIT2 := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIT2))
	assert.NotNil(t, aIT2)
	aIPB := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIPB))
	assert.NotNil(t, aIPB)

	errBody1, err := json.Marshal(model.APIAllocationUpdateRequest{Name: cutil.GetPtr("a")})
	assert.Nil(t, err)

	errBodyNameUniqueness, err := json.Marshal(model.APIAllocationUpdateRequest{Name: cutil.GetPtr("test-allocation-2")})
	assert.Nil(t, err)

	okBody1, err := json.Marshal(model.APIAllocationUpdateRequest{Name: cutil.GetPtr("UpdatedName1")})
	assert.Nil(t, err)
	okBody2, err := json.Marshal(model.APIAllocationUpdateRequest{Name: cutil.GetPtr("UpdatedName2"), Description: cutil.GetPtr("UpdatedDesc2")})
	assert.Nil(t, err)
	okBody3, err := json.Marshal(model.APIAllocationUpdateRequest{Name: cutil.GetPtr("test-allocation")})
	assert.Nil(t, err)
	okBody4, err := json.Marshal(model.APIAllocationUpdateRequest{Name: cutil.GetPtr("test-updated-allocation-3")})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock Temporal call
	tmc1 := &tmocks.Client{}
	wid1 := "test-workflow-id"
	wrun1 := &tmocks.WorkflowRun{}
	wrun1.On("GetID").Return(wid1)

	tmc1.Mock.On("ExecuteWorkflow", mock.Anything,
		mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, *uuid.UUID, *uuid.UUID, *uuid.UUID) error"),
		mock.AnythingOfType("*uuid.UUID"), mock.AnythingOfType("*uuid.UUID"), mock.AnythingOfType("*uuid.UUID")).Return(wrun1, nil)

	// Mock Temporal call
	tmc2 := &tmocks.Client{}
	wid2 := "test-workflow-id"
	wrun2 := &tmocks.WorkflowRun{}
	wrun2.On("GetID").Return(wid2)

	tmc2.Mock.On("ExecuteWorkflow", mock.Anything,
		mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, *uuid.UUID, *uuid.UUID, *uuid.UUID) error"),
		mock.AnythingOfType("*uuid.UUID"), mock.AnythingOfType("*uuid.UUID"), mock.AnythingOfType("*uuid.UUID")).Return(wrun2, fmt.Errorf("Failed to execute workflow"))

	tests := []struct {
		name                      string
		reqOrgName                string
		reqBody                   string
		user                      *cdbm.User
		aID                       string
		expectedErr               bool
		expectedStatus            int
		expectedName              string
		expectedDesc              *string
		verifyDerivedResourceName bool
		verifyChildSpanner        bool
		tmc                       *tmocks.Client
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody1),
			user:           nil,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			reqBody:        string(okBody1),
			user:           ipu,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when reqBody doesnt bind",
			reqOrgName:     ipOrg1,
			reqBody:        "BadBody",
			user:           ipu,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when reqBody json doesnt validate",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBody1),
			user:           ipu,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified org does not have infrastructure provider",
			reqOrgName:     ipOrg3,
			reqBody:        string(okBody1),
			user:           ipu,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when specified org does not have infrastructure provider matching the one in allocation",
			reqOrgName:     ipOrg2,
			reqBody:        string(okBody1),
			user:           ipu,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified allocation id is invalid uuid",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody1),
			user:           ipu,
			aID:            "bad$uuid",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified allocation doesnt exist",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody1),
			user:           ipu,
			aID:            uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when user doesn't have the right role",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody1),
			user:           tnu,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when updated name already exists for another allocation",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyNameUniqueness),
			user:           ipu,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:               "success case 1",
			reqOrgName:         ipOrg1,
			reqBody:            string(okBody1),
			user:               ipu,
			aID:                aIT.ID,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedName:       "UpdatedName1",
			expectedDesc:       nil,
			verifyChildSpanner: true,
			tmc:                tempClient,
		},
		{
			name:           "success case 2",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody2),
			user:           ipu,
			aID:            aIT.ID,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedName:   "UpdatedName2",
			expectedDesc:   cutil.GetPtr("UpdatedDesc2"),
			tmc:            tmc1,
		},
		{
			name:           "success when same name is sent for update",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBody3),
			user:           ipu,
			aID:            aIT.ID,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedName:   "test-allocation",
		},
		{
			name:                      "success when updating allocation with IP Block constraint",
			reqOrgName:                ipOrg1,
			reqBody:                   string(okBody4),
			user:                      ipu,
			aID:                       aIPB.ID,
			expectedErr:               false,
			expectedStatus:            http.StatusOK,
			expectedName:              "test-updated-allocation-3",
			verifyDerivedResourceName: true,
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
			values := []string{tc.reqOrgName, tc.aID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := UpdateAllocationHandler{
				dbSession: dbSession,
				tc:        tc.tmc,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APIAllocation{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, 1, len(rsp.StatusHistory))
				assert.Equal(t, 1, len(rsp.AllocationConstraints))
				assert.Equal(t, tc.expectedName, rsp.Name)
				if tc.expectedDesc != nil {
					assert.Equal(t, *tc.expectedDesc, *rsp.Description)
				}
				assert.NotEqual(t, aIT.Updated.String(), rsp.Updated.String())

				if len(rsp.AllocationConstraints) > 0 {
					if rsp.AllocationConstraints[0].ResourceType == cdbm.AllocationResourceTypeInstanceType {
						assert.NotNil(t, rsp.AllocationConstraints[0].InstanceType)
					}
					if rsp.AllocationConstraints[0].ResourceType == cdbm.AllocationResourceTypeIPBlock {
						assert.NotNil(t, rsp.AllocationConstraints[0].IPBlock)
					}
				}

				if tc.verifyDerivedResourceName {
					ipbDAO := cdbm.NewIPBlockDAO(dbSession)
					ipb, err := ipbDAO.GetByID(context.Background(), nil, uuid.MustParse(*rsp.AllocationConstraints[0].DerivedResourceID), nil)
					assert.NoError(t, err)
					assert.Equal(t, tc.expectedName, ipb.Name)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestAllocationHandler_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"

	ipRoles := []string{authz.ProviderAdminRole}
	ipu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)

	tnRoles := []string{authz.TenantAdminRole}
	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2}, tnRoles)

	ip := common.TestBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, ipu)
	ip2 := common.TestBuildInfrastructureProvider(t, dbSession, "TestIp2", ipOrg2, ipu)
	assert.NotNil(t, ip2)
	assert.NotNil(t, ip)

	site := testIPBlockBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)

	tenant1 := common.TestBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnu)

	cfg := common.GetTestConfig()
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	it1 := common.TestBuildInstanceType(t, dbSession, "testIT", cutil.GetPtr(uuid.New()), site, map[string]string{
		"name":        "test-instance-type-1",
		"description": "Test Instance Type 1 Description",
	}, ipu)
	// build some machines, and map the machines to the instancetypes
	for i := 1; i <= 5; i++ {
		mc := testInstanceBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(false), nil)
		assert.NotNil(t, mc)
		mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc, it1)
		assert.NotNil(t, mcinst1)
	}

	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	ipbFG := testIPBlockBuildIPBlock(t, dbSession, "testipbFG", site, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.170.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	ipbVpcPrefix := testIPBlockBuildIPBlock(t, dbSession, "testipbVpcPrefix", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.169.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)

	vpc1 := testAllocationBuildVpc(t, dbSession, ip, site, tenant1, ipOrg1, "testVPC")
	os1 := testAllocationBuildOperatingSystem(t, dbSession, "ubuntu")

	acGoodIT := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 2}
	acGoodITSmall := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 1}
	acGoodIPB := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipb1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}

	acGoodIPBVpcPrefix := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipbVpcPrefix.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 16}

	acGoodIPBFG := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipbFG.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 16}

	okBodyIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okit", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	okBodyRepeatedITInAC, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok-repeated", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	okBodySmallIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok-small", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodITSmall}})
	assert.Nil(t, err)
	okBodyIPB, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okipb", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPB}})
	assert.Nil(t, err)
	okBodyIPBFG, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okipbfg", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPBFG}})
	assert.Nil(t, err)
	okBodyIPBVpcPrefix, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okipbvpcprefix", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPBVpcPrefix}})
	assert.Nil(t, err)

	parentPref, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref)

	parentPrefFG, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbFG.Prefix, ipbFG.PrefixLength, ipbFG.RoutingType, ipbFG.InfrastructureProviderID.String(), ipbFG.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPrefFG)

	parentPrefVpcPrefix, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbVpcPrefix.Prefix, ipbVpcPrefix.PrefixLength, ipbVpcPrefix.RoutingType, ipbVpcPrefix.InfrastructureProviderID.String(), ipbVpcPrefix.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPrefVpcPrefix)

	// Create 1 Instance Type Allocation
	aIT := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIT))
	_ = uuid.MustParse(aIT.ID)
	_ = uuid.MustParse(aIT.AllocationConstraints[0].ID)
	// Create another instance type allocation with same instane type
	aIT2 := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyRepeatedITInAC))
	aIT3 := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodySmallIT))

	// Create 3 IP Block Allocation
	aIPB := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIPB))
	aIPBFG := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIPBFG))
	aIPBVpcPrefix := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okBodyIPBVpcPrefix))
	assert.NotNil(t, aIPBVpcPrefix)

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
	instance3, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "testInst3",
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
	instance4, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "testInst4",
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

	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)
	subnetDAO := cdbm.NewSubnetDAO(dbSession)

	// Subnet for IP Block
	childIPBUUID := uuid.MustParse(*aIPB.AllocationConstraints[0].DerivedResourceID)
	ipbDAO := cdbm.NewIPBlockDAO(dbSession)
	childIPB, err := ipbDAO.GetByID(ctx, nil, childIPBUUID, nil)
	assert.Nil(t, err)
	subnet := testAllocationBuildSubnet(t, dbSession, tenant1, vpc1, "testSubnet", childIPB.Prefix, childIPB)

	// VPC Prefix for IP Block
	childVpcPrefixUUID := uuid.MustParse(*aIPBVpcPrefix.AllocationConstraints[0].DerivedResourceID)
	childVpcPrefixIBP, err := ipbDAO.GetByID(ctx, nil, childVpcPrefixUUID, nil)
	assert.Nil(t, err)
	vpcPrefix := testAllocationBuildVpcPrefix(t, dbSession, tenant1, vpc1, "testVPCPrefix", childVpcPrefixIBP)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock Temporal call
	tmc1 := &tmocks.Client{}
	wid1 := "test-workflow-id"
	wrun1 := &tmocks.WorkflowRun{}
	wrun1.On("GetID").Return(wid1)

	tmc1.Mock.On("ExecuteWorkflow", mock.Anything,
		mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID) error"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun1, nil)

	// Mock Temporal call
	tmc2 := &tmocks.Client{}
	wid2 := "test-workflow-id"
	wrun2 := &tmocks.WorkflowRun{}
	wrun2.On("GetID").Return(wid2)

	tmc2.Mock.On("ExecuteWorkflow", mock.Anything,
		mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID) error"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun2, fmt.Errorf("Failed to execute workflow"))

	tmc3 := &tmocks.Client{}
	wid3 := "test-workflow-id"
	wrun3 := &tmocks.WorkflowRun{}
	wrun3.On("GetID").Return(wid3)

	tmc3.Mock.On("ExecuteWorkflow", mock.Anything,
		mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID) error"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun3, nil)

	tests := []struct {
		name               string
		reqOrgName         string
		user               *cdbm.User
		aID                string
		allocation         *model.APIAllocation
		expectedErr        bool
		expectedStatus     int
		deleteInstanceIDs  []uuid.UUID
		deleteSubnetID     *uuid.UUID
		deleteVpcPrefixID  *uuid.UUID
		checkFullGrant     bool
		verifyChildSpanner bool
		tenantSiteCount    int
		tmc                *tmocks.Client
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			aID:            aIT.ID,
			allocation:     aIT,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           ipu,
			aID:            aIT.ID,
			allocation:     aIT,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when allocation id is invalid uuid",
			reqOrgName:     ipOrg1,
			user:           ipu,
			aID:            "bad#uuid$str",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified org does not have infrastructure provider",
			reqOrgName:     ipOrg3,
			user:           ipu,
			aID:            aIT.ID,
			allocation:     aIT,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified allocation doesnt exist",
			reqOrgName:     ipOrg1,
			user:           ipu,
			aID:            uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when org's infrastructure provider does not match alock's infrastructure provider",
			reqOrgName:     ipOrg2,
			user:           ipu,
			aID:            aIT.ID,
			allocation:     aIT,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when tenant has instances using instance type",
			reqOrgName:     ipOrg1,
			user:           ipu,
			aID:            aIT.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			deleteInstanceIDs: []uuid.UUID{
				instance3.ID,
				instance4.ID,
			},
		},
		{
			name:            "success when deleting allocation still leaves enough aggregate capacity",
			reqOrgName:      ipOrg1,
			user:            ipu,
			aID:             aIT3.ID,
			allocation:      aIT3,
			expectedErr:     false,
			expectedStatus:  http.StatusAccepted,
			tenantSiteCount: 1,
			deleteInstanceIDs: []uuid.UUID{
				instance2.ID,
			},
		},
		{
			name:           "error when user doesn't have the right role",
			reqOrgName:     tnOrg1,
			user:           tnu,
			aID:            aIT.ID,
			allocation:     aIT,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant has subnets using IPBlock",
			reqOrgName:     ipOrg1,
			user:           ipu,
			aID:            aIPB.ID,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			deleteSubnetID: &subnet.ID,
		},
		{
			name:              "error when tenant has VpcPrefixes using IPBlock",
			reqOrgName:        ipOrg1,
			user:              ipu,
			aID:               aIPBVpcPrefix.ID,
			expectedErr:       true,
			expectedStatus:    http.StatusBadRequest,
			deleteVpcPrefixID: &vpcPrefix.ID,
		},
		{
			name:               "success case IPBlock with Subnet as Allocation Constraint",
			reqOrgName:         ipOrg1,
			user:               ipu,
			aID:                aIPB.ID,
			allocation:         aIPB,
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
			tenantSiteCount:    1, // Allocations left
			tmc:                tmc1,
		},
		{
			name:               "success case IPBlock with VPC Prefix as Allocation Constraint",
			reqOrgName:         ipOrg1,
			user:               ipu,
			aID:                aIPBVpcPrefix.ID,
			allocation:         aIPBVpcPrefix,
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
			tenantSiteCount:    1, // Allocations left
			tmc:                tmc3,
		},
		{
			name:            "success case with IPBlock with Full Grant which should be cleared",
			reqOrgName:      ipOrg1,
			user:            ipu,
			aID:             aIPBFG.ID,
			allocation:      aIPBFG,
			expectedErr:     false,
			expectedStatus:  http.StatusAccepted,
			checkFullGrant:  true,
			tenantSiteCount: 1, // Allocations left
			tmc:             tmc2,
		},
		{
			name:            "success case with InstanceType, should cause an update of the instance type",
			reqOrgName:      ipOrg1,
			user:            ipu,
			aID:             aIT2.ID,
			allocation:      aIT2,
			expectedErr:     false,
			expectedStatus:  http.StatusAccepted,
			tenantSiteCount: 1, // allocation left
			deleteInstanceIDs: []uuid.UUID{
				instance.ID,
			},
		},
		{
			name:            "success case with InstanceType, should cause deletion",
			reqOrgName:      ipOrg1,
			user:            ipu,
			aID:             aIT.ID,
			allocation:      aIT,
			expectedErr:     false,
			expectedStatus:  http.StatusAccepted,
			tenantSiteCount: 0, // no allocations left
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
			values := []string{tc.reqOrgName, tc.aID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			dah := DeleteAllocationHandler{
				dbSession: dbSession,
				tc:        tc.tmc,
				cfg:       cfg,
			}
			err := dah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusAccepted)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				// verify allocation deleted
				aDAO := cdbm.NewAllocationDAO(dbSession)
				id1, err := uuid.Parse(tc.allocation.ID)
				assert.Nil(t, err)
				_, err = aDAO.GetByID(ctx, nil, id1, nil)
				assert.NotNil(t, err)
				// verify allocation constraint deleted
				acDAO := cdbm.NewAllocationConstraintDAO(dbSession)
				id2, err := uuid.Parse(tc.allocation.AllocationConstraints[0].ID)
				assert.Nil(t, err)
				_, err = acDAO.GetByID(ctx, nil, id2, nil)
				assert.NotNil(t, err)

				if tc.checkFullGrant {
					// verify full grant is cleared
					ipbfg, err := ipbDAO.GetByID(ctx, nil, ipbFG.ID, nil)
					assert.Nil(t, err)
					assert.Equal(t, false, ipbfg.FullGrant)
				}

				// Check Tenant/Site association
				tsDAO := cdbm.NewTenantSiteDAO(dbSession)
				tenantID := uuid.MustParse(tc.allocation.TenantID)
				siteID := uuid.MustParse(tc.allocation.SiteID)
				_, tscount, err := tsDAO.GetAll(
					ctx,
					nil,
					cdbm.TenantSiteFilterInput{
						TenantIDs: []uuid.UUID{tenantID},
						SiteIDs:   []uuid.UUID{siteID},
					},
					cdbp.PageInput{},
					nil,
				)
				assert.Nil(t, err)
				assert.Equal(t, tc.tenantSiteCount, tscount)
			}

			for _, deleteInstanceID := range tc.deleteInstanceIDs {
				err = instanceDAO.Delete(ctx, nil, deleteInstanceID)
				assert.Nil(t, err)
			}
			if tc.deleteSubnetID != nil {
				err = subnetDAO.Delete(ctx, nil, *tc.deleteSubnetID)
				assert.Nil(t, err)
			}

			if tc.deleteVpcPrefixID != nil {
				err = vpcPrefixDAO.Delete(ctx, nil, *tc.deleteVpcPrefixID)
				assert.Nil(t, err)
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInstanceTypeAllocationForMultipleTenants(t *testing.T) {
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

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	it1 := testMachineBuildInstanceType(t, dbSession, ip, site, "testIT")

	// build some machines, and map the machines to the instancetypes
	for i := 1; i <= 10; i++ {
		mc := testInstanceBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(false), nil)
		assert.NotNil(t, mc)
		mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc, it1)
		assert.NotNil(t, mcinst1)
	}

	acGoodIT := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 6}
	acGoodIT2 := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 4}

	okBodyIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)

	okBodyITForDifferentTenant, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), TenantID: tenant2.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT2}})
	assert.Nil(t, err)

	// Mock Temporal Site Client pool
	tsc := &tmocks.Client{}

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site.ID.String()] = tsc
	scp.IDClientMap[site2.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateTenant", mock.Anything).Return(wrun, nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                 string
		reqOrgName           string
		reqBody              string
		user                 *cdbm.User
		expectedErr          bool
		expectedStatus       int
		expectedIpam         bool
		expectedInstanceType bool
		verifyChildSpanner   bool
	}{
		{
			name:                 "success allocating 6 machines for first tenant",
			reqOrgName:           ipOrg1,
			reqBody:              string(okBodyIT),
			user:                 ipu,
			expectedErr:          false,
			expectedStatus:       http.StatusCreated,
			expectedInstanceType: true,
			verifyChildSpanner:   true,
		},
		{
			name:                 "success allocating 4 machines for second tenant",
			reqOrgName:           ipOrg1,
			reqBody:              string(okBodyITForDifferentTenant),
			user:                 ipu,
			expectedErr:          false,
			expectedStatus:       http.StatusCreated,
			expectedInstanceType: true,
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

			cipbh := CreateAllocationHandler{
				dbSession: dbSession,
				tc:        tempClient,
				scp:       scp,
				cfg:       cfg,
			}
			err := cipbh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusCreated)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APIAllocation{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, len(rsp.StatusHistory), 1)
				// validate allocation constraint record exists
				assert.Equal(t, 1, len(rsp.AllocationConstraints))

				if tc.expectedInstanceType {
					childIT := rsp.AllocationConstraints[0].ID
					childITUUID, err := uuid.Parse(childIT)
					assert.Nil(t, err)
					acDAO := cdbm.NewAllocationConstraintDAO(dbSession)
					_, err = acDAO.GetByID(ctx, nil, childITUUID, nil)
					assert.Nil(t, err)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
