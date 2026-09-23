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

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	oteltrace "go.opentelemetry.io/otel/trace"
	tmocks "go.temporal.io/sdk/mocks"
)

func TestAllocationConstraintHandler_Update(t *testing.T) {
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
	tenant2 := common.TestBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, tnu)

	cfg := common.GetTestConfig()

	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	// Setup Instance Types
	it1 := common.TestBuildInstanceType(t, dbSession, "testIT", cutil.GetPtr(uuid.New()), site, map[string]string{
		"name":        "test-instance-type-1",
		"description": "Test Instance Type 1 Description",
	}, ipu)
	it2 := common.TestBuildInstanceType(t, dbSession, "testIT2", cutil.GetPtr(uuid.New()), site, map[string]string{
		"name":        "test-instance-type-2",
		"description": "Test Instance Type 2 Description",
	}, ipu)

	// Build enough Machines for Instance Type it1 so global reserved Allocation Constraints
	// (this test's allocations plus tenant2's it1 allocation) stay below the Machine count when
	// CheckMachinesForInstanceTypeAllocation adds an increase delta.
	for i := 1; i <= 40; i++ {
		mc := testInstanceBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(false), nil)
		assert.NotNil(t, mc)
		mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc, it1)
		assert.NotNil(t, mcinst1)

		mc2 := testInstanceBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(false), nil)
		assert.NotNil(t, mc2)
		mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc2, it2)
		assert.NotNil(t, mcinst2)
	}

	// Setup IP Blocks
	ipb1 := testIPBlockBuildIPBlock(t, dbSession, "testipb", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	ipb2 := testIPBlockBuildIPBlock(t, dbSession, "testipb2", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.167.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	ipb3 := testIPBlockBuildIPBlock(t, dbSession, "testipb3", site, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "10.100.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	operatorRoot := testIPBlockBuildIPBlock(t, dbSession, "operator-root", site, ip, nil, cdbm.IPBlockRoutingTypeDatacenterOnly, "172.16.0.0", 16, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)

	parentPref1, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref1)

	parentPref, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb2.Prefix, ipb2.PrefixLength, ipb2.RoutingType, ipb2.InfrastructureProviderID.String(), ipb2.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref)

	parentPref3, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb3.Prefix, ipb3.PrefixLength, ipb3.RoutingType, ipb3.InfrastructureProviderID.String(), ipb3.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, parentPref3)

	operatorRootPrefix, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, operatorRoot.Prefix, operatorRoot.PrefixLength, operatorRoot.RoutingType, operatorRoot.InfrastructureProviderID.String(), operatorRoot.SiteID.String())
	assert.Nil(t, err)
	assert.NotNil(t, operatorRootPrefix)

	// Setup Allocation/Constraints
	acGoodIT1 := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 22}
	acGoodIPB1 := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipb1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}

	okABodyIT1, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okit1", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT1}})
	assert.Nil(t, err)
	okABodyIPB1, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okipb1", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPB1}})
	assert.Nil(t, err)

	acGoodIT2 := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it2.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 22}
	acGoodIPB2 := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipb2.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}
	acGoodIPB3 := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: ipb3.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}
	acOperatorRoot := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: operatorRoot.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 24}
	acGoodITTenant2 := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it1.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 7}

	okABodyIT2, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okit2", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT2}})
	assert.Nil(t, err)
	okABodyIPB2, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okipb2", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPB2}})
	assert.Nil(t, err)
	okABodyIPB3, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okipb3", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIPB3}})
	assert.Nil(t, err)
	operatorRootAllocationBody, err := json.Marshal(model.APIAllocationCreateRequest{Name: "operator-root-allocation", Description: cutil.GetPtr(""), TenantID: tenant1.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acOperatorRoot}})
	assert.Nil(t, err)
	okABodyITTenant2, err := json.Marshal(model.APIAllocationCreateRequest{Name: "okit-tenant-2", Description: cutil.GetPtr(""), TenantID: tenant2.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodITTenant2}})
	assert.Nil(t, err)

	// Allocation 1
	aIT1 := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okABodyIT1))
	aID1, _ := uuid.Parse(aIT1.ID)
	assert.NotNil(t, aID1)
	acID1 := uuid.MustParse(aIT1.AllocationConstraints[0].ID)
	assert.NotNil(t, acID1)

	aIPB1 := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okABodyIPB1))
	aipID1, _ := uuid.Parse(aIPB1.ID)
	assert.NotNil(t, aipID1)
	acipID1 := uuid.MustParse(aIPB1.AllocationConstraints[0].ID)
	assert.NotNil(t, acipID1)

	// Allocation 2
	aIT2 := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okABodyIT2))
	aID2, _ := uuid.Parse(aIT2.ID)
	assert.NotNil(t, aID2)
	acID2 := uuid.MustParse(aIT2.AllocationConstraints[0].ID)
	assert.NotNil(t, acID2)

	aIPB2 := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okABodyIPB2))
	aipID2, _ := uuid.Parse(aIPB2.ID)
	assert.NotNil(t, aipID2)
	acipID2 := uuid.MustParse(aIPB2.AllocationConstraints[0].ID)
	assert.NotNil(t, acipID2)

	aIPB3 := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okABodyIPB3))
	aipID3, _ := uuid.Parse(aIPB3.ID)
	assert.NotNil(t, aipID3)

	operatorRootAllocation := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(operatorRootAllocationBody))
	operatorRootAllocationID := uuid.MustParse(operatorRootAllocation.ID)

	aITTenant2 := testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg1, string(okABodyITTenant2))
	assert.NotNil(t, aITTenant2)

	// Get Allocation Constraints for above Allocations
	acDAO := cdbm.NewAllocationConstraintDAO(dbSession)
	acsit1, _, err := acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{AllocationIDs: []uuid.UUID{aID1}}, cdbp.PageInput{}, nil)
	assert.Nil(t, err)
	assert.NotNil(t, acsit1)

	acsip1, _, err := acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{AllocationIDs: []uuid.UUID{aipID1}}, cdbp.PageInput{}, nil)
	assert.Nil(t, err)
	assert.NotNil(t, acsip1)

	acsit2, _, err := acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{AllocationIDs: []uuid.UUID{aID2}}, cdbp.PageInput{}, nil)
	assert.Nil(t, err)
	assert.NotNil(t, acsit2)

	acsip2, _, err := acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{AllocationIDs: []uuid.UUID{aipID2}}, cdbp.PageInput{}, nil)
	assert.Nil(t, err)
	assert.NotNil(t, acsip2)

	acsip3, _, err := acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{AllocationIDs: []uuid.UUID{aipID3}}, cdbp.PageInput{}, nil)
	assert.Nil(t, err)
	assert.NotNil(t, acsip3)

	operatorRootConstraints, _, err := acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{AllocationIDs: []uuid.UUID{operatorRootAllocationID}}, cdbp.PageInput{}, nil)
	require.NoError(t, err)
	require.Len(t, operatorRootConstraints, 1)

	// Setup test data for Allocation Constraint Update
	okBodyIT1, err := json.Marshal(model.APIAllocationConstraintUpdateRequest{ConstraintValue: 23})
	assert.Nil(t, err)

	errBodyIT1, err := json.Marshal(model.APIAllocationConstraintUpdateRequest{ConstraintValue: 50})
	assert.Nil(t, err)

	okBodyIP1, err := json.Marshal(model.APIAllocationConstraintUpdateRequest{ConstraintValue: 26})
	assert.Nil(t, err)

	okBodyIP1ToFG, err := json.Marshal(model.APIAllocationConstraintUpdateRequest{ConstraintValue: 16})
	assert.Nil(t, err)

	okBodyIT2, err := json.Marshal(model.APIAllocationConstraintUpdateRequest{ConstraintValue: 24})
	assert.Nil(t, err)

	okBodyIT3, err := json.Marshal(model.APIAllocationConstraintUpdateRequest{ConstraintValue: 1})
	assert.Nil(t, err)

	okBodyIT4, err := json.Marshal(model.APIAllocationConstraintUpdateRequest{ConstraintValue: 22})
	assert.Nil(t, err)

	okBodyIP2, err := json.Marshal(model.APIAllocationConstraintUpdateRequest{ConstraintValue: 26})
	assert.Nil(t, err)

	errBodyIP1, err := json.Marshal(model.APIAllocationConstraintUpdateRequest{ConstraintValue: 100})
	assert.Nil(t, err)

	vpc1 := testAllocationBuildVpc(t, dbSession, ip, site, tenant1, ipOrg1, "testVPC")
	os1 := testAllocationBuildOperatingSystem(t, dbSession, "ubuntu")

	// Create test instances
	instanceDAO := cdbm.NewInstanceDAO(dbSession)

	for i := 1; i <= 22; i += 1 {
		instance, err := instanceDAO.Create(
			ctx, nil,
			cdbm.InstanceCreateInput{
				Name:                     fmt.Sprintf("testInst-%v", i),
				TenantID:                 tenant1.ID,
				InfrastructureProviderID: ip.ID,
				SiteID:                   site.ID,
				InstanceTypeID:           &it1.ID,
				VpcID:                    vpc1.ID,
				OperatingSystemID:        &os1.ID,
				Status:                   cdbm.InstanceStatusReady,
				CreatedBy:                tnu.ID,
			},
		)
		assert.Nil(t, err)
		assert.NotNil(t, instance)
	}

	// Create test subnet
	childIPBUUID := uuid.MustParse(*aIPB2.AllocationConstraints[0].DerivedResourceID)
	ipbDAO := cdbm.NewIPBlockDAO(dbSession)
	childIPB, err := ipbDAO.GetByID(ctx, nil, childIPBUUID, nil)
	assert.Nil(t, err)

	for i := 1; i <= 22; i += 1 {
		subnet := testAllocationBuildSubnet(t, dbSession, tenant1, vpc1, fmt.Sprintf("testSubnet-%v", i), childIPB.Prefix, childIPB)
		assert.NotNil(t, subnet)
	}

	childIPB3UUID := uuid.MustParse(*aIPB3.AllocationConstraints[0].DerivedResourceID)
	childIPB3, err := ipbDAO.GetByID(ctx, nil, childIPB3UUID, nil)
	assert.Nil(t, err)
	vpcPrefixForAC := testAllocationBuildVpcPrefix(t, dbSession, tenant1, vpc1, "testVPCPrefix-ac-update", childIPB3)
	assert.NotNil(t, vpcPrefixForAC)

	operatorRootConstraint := operatorRootConstraints[0]
	require.NotNil(t, operatorRootConstraint.DerivedResourceID)
	operatorChildID := *operatorRootConstraint.DerivedResourceID
	operatorChild, err := ipbDAO.GetByID(ctx, nil, operatorChildID, nil)
	require.NoError(t, err)
	operatorChildBeforeUpdate := *operatorChild

	operatorRoot, err = ipbDAO.LinkSitePrefix(ctx, nil, operatorRoot.ID, uuid.New())
	require.NoError(t, err)
	operatorRoot, err = ipbDAO.Update(ctx, nil, cdbm.IPBlockUpdateInput{
		IPBlockID: operatorRoot.ID,
		Status:    cutil.GetPtr(cdbm.IPBlockStatusDeleting),
	})
	require.NoError(t, err)

	operatorRootUsageBeforeUpdate, err := ipam.GetIpamUsageForIPBlock(ctx, ipamStorage, operatorRoot)
	require.NoError(t, err)
	operatorRootNamespace := ipam.GetIpamNamespaceForIPBlock(ctx, operatorRoot.RoutingType, operatorRoot.InfrastructureProviderID.String(), operatorRoot.SiteID.String())
	operatorChildCIDR := ipam.GetCidrForIPBlock(ctx, operatorChild.Prefix, operatorChild.PrefixLength)
	operatorChildPrefixBeforeUpdate, err := ipamStorage.ReadPrefix(ctx, operatorChildCIDR, operatorRootNamespace)
	require.NoError(t, err)

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
		name                    string
		reqOrgName              string
		reqBody                 string
		user                    *cdbm.User
		acID                    string
		requestedAID            uuid.UUID
		requestedACS            cdbm.AllocationConstraint
		expectedErr             bool
		expectedErrMessage      string
		expectedIpamErrMsg      string
		expectedStatus          int
		expectedConstraintValue int
		checkFullGrant          *bool
		verifyChildSpanner      bool
		tmc                     *tmocks.Client
		assertState             func(t *testing.T)
	}{
		{
			name:           "error when User is not found in Request Context",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBodyIT1),
			user:           nil,
			requestedAID:   acsit1[0].AllocationID,
			requestedACS:   acsit1[0],
			acID:           acsit1[0].ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when User does not belong to Organization",
			reqOrgName:     "SomeOrg",
			reqBody:        string(okBodyIT1),
			user:           ipu,
			requestedAID:   acsit1[0].AllocationID,
			requestedACS:   acsit1[0],
			acID:           acsit1[0].ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when Request Body does not bind",
			reqOrgName:     ipOrg1,
			reqBody:        "BadBody",
			user:           ipu,
			requestedAID:   acsit1[0].AllocationID,
			requestedACS:   acsit1[0],
			acID:           acsit1[0].ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when specified Organization has no Infrastructure Provider",
			reqOrgName:     ipOrg3,
			reqBody:        string(okBodyIT1),
			user:           ipu,
			requestedAID:   acsit1[0].AllocationID,
			requestedACS:   acsit1[0],
			acID:           acsit1[0].ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Organization's Infrastructure Provider does not match Allocation's",
			reqOrgName:     ipOrg2,
			reqBody:        string(okBodyIT1),
			user:           ipu,
			requestedAID:   acsit1[0].AllocationID,
			requestedACS:   acsit1[0],
			acID:           acsit1[0].ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Allocation Constraint ID is not a valid UUID",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBodyIT1),
			user:           ipu,
			requestedAID:   acsit1[0].AllocationID,
			requestedACS:   acsit1[0],
			acID:           "bad$uuid",
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Allocation Constraint does not exist",
			reqOrgName:     ipOrg1,
			reqBody:        string(okBodyIT1),
			user:           ipu,
			requestedAID:   acsit1[0].AllocationID,
			requestedACS:   acsit1[0],
			acID:           uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when User does not have the required role",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBodyIT1),
			user:           tnu,
			requestedAID:   acsit1[0].AllocationID,
			requestedACS:   acsit1[0],
			acID:           acsit1[0].ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:               "error when Allocation ID does not match Allocation Constraint",
			reqOrgName:         ipOrg1,
			reqBody:            string(okBodyIT1),
			user:               ipu,
			requestedAID:       uuid.New(),
			requestedACS:       acsit1[0],
			acID:               acsit1[0].ID.String(),
			expectedErr:        true,
			expectedStatus:     http.StatusBadRequest,
			expectedErrMessage: "Allocation Constraint does not belong to Allocation specified in request",
		},
		{
			name:                    "success updating Allocation Constraint value for Instance Type resource",
			reqOrgName:              ipOrg1,
			reqBody:                 string(okBodyIT1),
			user:                    ipu,
			requestedAID:            acsit1[0].AllocationID,
			requestedACS:            acsit1[0],
			acID:                    acsit1[0].ID.String(),
			expectedErr:             false,
			expectedStatus:          http.StatusOK,
			expectedConstraintValue: 23,
			verifyChildSpanner:      true,
		},
		{
			name:           "error when Machines are not available for Allocation Constraint update",
			reqOrgName:     ipOrg1,
			reqBody:        string(errBodyIT1),
			user:           ipu,
			requestedAID:   acsit1[0].AllocationID,
			requestedACS:   acsit1[0],
			acID:           acsit1[0].ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:                    "successfully updating Allocation Constraint value for IP Block resource",
			reqOrgName:              ipOrg1,
			reqBody:                 string(okBodyIP1),
			user:                    ipu,
			requestedAID:            acsip1[0].AllocationID,
			requestedACS:            acsip1[0],
			acID:                    acsip1[0].ID.String(),
			expectedErr:             false,
			expectedStatus:          http.StatusOK,
			expectedConstraintValue: 26,
			tmc:                     tmc1,
		},
		{
			name:                    "successfully updating Allocation Constraint value for IP Block resource with full grant",
			reqOrgName:              ipOrg1,
			reqBody:                 string(okBodyIP1ToFG),
			user:                    ipu,
			requestedAID:            acsip1[0].AllocationID,
			requestedACS:            acsip1[0],
			acID:                    acsip1[0].ID.String(),
			expectedErr:             false,
			expectedStatus:          http.StatusOK,
			expectedConstraintValue: 16,
			checkFullGrant:          cutil.GetPtr(true),
		},
		{
			name:                    "successfully updating Allocation Constraint value for IP Block resource away from full grant",
			reqOrgName:              ipOrg1,
			reqBody:                 string(okBodyIP1),
			user:                    ipu,
			requestedAID:            acsip1[0].AllocationID,
			requestedACS:            acsip1[0],
			acID:                    acsip1[0].ID.String(),
			expectedErr:             false,
			expectedStatus:          http.StatusOK,
			expectedConstraintValue: 26,
			checkFullGrant:          cutil.GetPtr(false),
		},
		{
			name:                    "success updating Allocation Constraint when Instance count is below new value",
			reqOrgName:              ipOrg1,
			reqBody:                 string(okBodyIT2),
			user:                    ipu,
			requestedAID:            acsit2[0].AllocationID,
			requestedACS:            acsit2[0],
			acID:                    acsit2[0].ID.String(),
			expectedErr:             false,
			expectedStatus:          http.StatusOK,
			expectedConstraintValue: 24,
		},
		{
			name:                    "success updating Allocation Constraint when Instance count equals new value",
			reqOrgName:              ipOrg1,
			reqBody:                 string(okBodyIT4),
			user:                    ipu,
			requestedAID:            acsit2[0].AllocationID,
			requestedACS:            acsit2[0],
			acID:                    acsit2[0].ID.String(),
			expectedErr:             false,
			expectedStatus:          http.StatusOK,
			expectedConstraintValue: 22,
		},
		{
			name:                    "error when Instance count exceeds new Allocation Constraint value",
			reqOrgName:              ipOrg1,
			reqBody:                 string(okBodyIT3),
			user:                    ipu,
			requestedAID:            acsit1[0].AllocationID,
			requestedACS:            acsit1[0],
			acID:                    acsit1[0].ID.String(),
			expectedErr:             true,
			expectedErrMessage:      "Updating this Allocation Constraint as specified would result in 1 total Machines for Instance Type: testIT allocated to Tenant, less than Tenant's active Instance count: 22 for the Instance Type",
			expectedStatus:          http.StatusBadRequest,
			expectedConstraintValue: 0,
		},
		{
			name:                    "error when Subnets exist for Allocation Constraint IP Block",
			reqOrgName:              ipOrg1,
			reqBody:                 string(okBodyIP2),
			user:                    ipu,
			requestedAID:            acsip2[0].AllocationID,
			requestedACS:            acsip2[0],
			acID:                    acsip2[0].ID.String(),
			expectedErr:             true,
			expectedErrMessage:      "Subnets exist for Allocation Constraint, cannot update constraint value",
			expectedStatus:          http.StatusBadRequest,
			expectedConstraintValue: 0,
		},
		{
			name:                    "error when VPC Prefix exists for Allocation Constraint child IP Block",
			reqOrgName:              ipOrg1,
			reqBody:                 string(okBodyIP2),
			user:                    ipu,
			requestedAID:            acsip3[0].AllocationID,
			requestedACS:            acsip3[0],
			acID:                    acsip3[0].ID.String(),
			expectedErr:             true,
			expectedErrMessage:      "VPC Prefixes exist for Allocation Constraint, cannot update constraint value",
			expectedStatus:          http.StatusBadRequest,
			expectedConstraintValue: 0,
		},
		{
			name:               "error when OperatorManaged SitePrefix parent IP Block is deleting",
			reqOrgName:         ipOrg1,
			reqBody:            string(okBodyIP1),
			user:               ipu,
			requestedAID:       operatorRootConstraint.AllocationID,
			requestedACS:       operatorRootConstraint,
			acID:               operatorRootConstraint.ID.String(),
			expectedErr:        true,
			expectedErrMessage: "linked to an OperatorManaged SitePrefix is not Ready",
			expectedStatus:     http.StatusBadRequest,
			assertState: func(t *testing.T) {
				constraintAfterUpdate, derr := acDAO.GetByID(ctx, nil, operatorRootConstraint.ID, nil)
				if assert.NoError(t, derr) {
					assert.Equal(t, operatorRootConstraint.ConstraintValue, constraintAfterUpdate.ConstraintValue)
					assert.Equal(t, operatorRootConstraint.DerivedResourceID, constraintAfterUpdate.DerivedResourceID)
				}

				childAfterUpdate, derr := ipbDAO.GetByID(ctx, nil, operatorChildID, nil)
				if assert.NoError(t, derr) {
					assert.Equal(t, operatorChildBeforeUpdate.ID, childAfterUpdate.ID)
					assert.Equal(t, operatorChildBeforeUpdate.Prefix, childAfterUpdate.Prefix)
					assert.Equal(t, operatorChildBeforeUpdate.PrefixLength, childAfterUpdate.PrefixLength)
					assert.Equal(t, operatorChildBeforeUpdate.Updated, childAfterUpdate.Updated)
				}

				operatorRootUsageAfterUpdate, derr := ipam.GetIpamUsageForIPBlock(ctx, ipamStorage, operatorRoot)
				if assert.NoError(t, derr) {
					assert.Equal(t, operatorRootUsageBeforeUpdate, operatorRootUsageAfterUpdate)
				}

				operatorChildPrefixAfterUpdate, derr := ipamStorage.ReadPrefix(ctx, operatorChildCIDR, operatorRootNamespace)
				if assert.NoError(t, derr) {
					assert.Equal(t, operatorChildPrefixBeforeUpdate.Cidr, operatorChildPrefixAfterUpdate.Cidr)
					assert.Equal(t, operatorChildPrefixBeforeUpdate.ParentCidr, operatorChildPrefixAfterUpdate.ParentCidr)
				}
			},
		},
		{
			name:               "error updating IP Block Allocation Constraint value due to IPAM error",
			reqOrgName:         ipOrg1,
			reqBody:            string(errBodyIP1),
			user:               ipu,
			requestedAID:       acsip1[0].AllocationID,
			requestedACS:       acsip1[0],
			acID:               acsip1[0].ID.String(),
			expectedErr:        true,
			expectedStatus:     http.StatusBadRequest,
			expectedIpamErrMsg: "Failed to create updated IPAM entry for Allocation Constraint's Tenant IP Block. Details: unable to persist created child:unable to parse cidr:invalid Prefix",
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
			names := []string{"orgName", "allocationId", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.requestedAID.String(), tc.acID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := UpdateAllocationConstraintHandler{
				dbSession: dbSession,
				tc:        tc.tmc,
				cfg:       cfg,
			}

			err := tah.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if tc.expectedErr && tc.expectedErrMessage != "" {
				assert.Contains(t, rec.Body.String(), tc.expectedErrMessage)
			}
			if tc.assertState != nil {
				tc.assertState(t)
			}
			if tc.expectedErr {
				if tc.expectedIpamErrMsg != "" {
					assert.Contains(t, rec.Body.String(), tc.expectedIpamErrMsg)
				}
				return
			}

			rsp := &model.APIAllocationConstraint{}
			err = json.Unmarshal(rec.Body.Bytes(), rsp)
			assert.Nil(t, err)

			// Validate response fields
			assert.Equal(t, tc.expectedConstraintValue, rsp.ConstraintValue)
			assert.NotEqual(t, tc.requestedACS.Updated, rsp.Updated)

			// Check full grant status for Allocation resource
			if tc.checkFullGrant != nil {
				ipb, serr := ipbDAO.GetByID(ctx, nil, uuid.MustParse(rsp.ResourceTypeID), nil)
				assert.Nil(t, serr)
				assert.Equal(t, *tc.checkFullGrant, ipb.FullGrant)
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
