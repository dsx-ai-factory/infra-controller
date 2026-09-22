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

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	tmocks "go.temporal.io/sdk/mocks"
)

func testBatchInstanceInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

// testBatchBuildMachineWithNVLinkDomain creates a machine with NVLink domain ID in Metadata
func testBatchBuildMachineWithNVLinkDomain(t *testing.T, dbSession *cdb.Session, ip uuid.UUID, site uuid.UUID, nvlinkDomainID string) *cdbm.Machine {
	mc := testInstanceBuildMachine(t, dbSession, ip, site, cutil.GetPtr(false), nil)
	mc.Metadata = &cdbm.SiteControllerMachine{
		Machine: &corev1.Machine{
			Status: &corev1.MachineStatus{
				NvlinkInfo: &corev1.MachineNVLinkInfo{
					DomainUuid: &corev1.NVLinkDomainId{
						Value: nvlinkDomainID,
					},
				},
			},
		},
	}
	_, err := dbSession.DB.NewUpdate().Model(mc).Column("metadata").Where("id = ?", mc.ID).Exec(context.Background())
	assert.Nil(t, err)
	return mc
}

func TestAllocateMachinesForBatch(t *testing.T) {
	dbSession := testBatchInstanceInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	ctx := context.Background()
	providerOrg := "test-label-filter-provider"
	providerUser := testInstanceBuildUser(t, dbSession, "test-label-filter-user", providerOrg, []string{authz.ProviderAdminRole})
	provider := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-label-filter-provider", providerOrg, providerUser)
	site := testInstanceBuildSite(t, dbSession, provider, "test-label-filter-site", cdbm.SiteStatusRegistered, true, providerUser)
	instanceType := testInstanceBuildInstanceType(t, dbSession, provider, "test-label-filter-type", site, cdbm.InstanceStatusReady)

	for i, failureDomain := range []string{"fd-a", "fd-a", "fd-b"} {
		machine := testBatchBuildMachineWithNVLinkDomain(t, dbSession, provider.ID, site.ID, "nvlink-domain-1")
		machine.Labels = map[string]string{
			"failure-domain": failureDomain,
			"power-domain":   fmt.Sprintf("pd-%d", i),
		}
		_, err := cdbm.NewMachineDAO(dbSession).Update(ctx, nil, cdbm.MachineUpdateInput{
			MachineID: machine.ID,
			Labels:    machine.Labels,
		})
		require.NoError(t, err)
		testInstanceBuildMachineInstanceType(t, dbSession, machine, instanceType)
	}

	tests := []struct {
		name                 string
		machineLabelSelector map[string]string
		wantStatus           int
		wantFailureDomain    string
	}{
		{
			name:                 "insufficient matching capacity",
			machineLabelSelector: map[string]string{"failure-domain": "missing"},
			wantStatus:           http.StatusConflict,
		},
		{
			name:                 "filters before topology optimization",
			machineLabelSelector: map[string]string{"failure-domain": "fd-a"},
			wantFailureDomain:    "fd-a",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			tx, err := cdb.BeginTx(ctx, dbSession, nil)
			require.NoError(t, err)
			defer func() {
				require.NoError(t, tx.Rollback())
			}()

			machines, apiErr := allocateMachinesForBatch(
				ctx, tx, dbSession, instanceType, 2, true,
				test.machineLabelSelector, zerolog.Nop(),
			)
			if test.wantStatus != 0 {
				require.NotNil(t, apiErr)
				assert.Equal(t, test.wantStatus, apiErr.Code)
				return
			}

			require.Nil(t, apiErr)
			require.Len(t, machines, 2)
			for _, machine := range machines {
				assert.Equal(t, test.wantFailureDomain, machine.Labels["failure-domain"])
			}
		})
	}
}

func TestBatchCreateInstanceHandler_Handle(t *testing.T) {
	dbSession := testBatchInstanceInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}

	// Infrastructure Provider User and Provider
	ipu := testInstanceBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	// Site 1 - Main test site
	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	// Site 2 - ready site for cross-site VPC prefix validation
	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st2)

	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant-1", tnOrg, tnu1)
	_ = common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip, &tn1.ID, tnOrg, cdbm.TenantAccountStatusReady, tnu1)

	// Tenant-Site association
	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)
	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2)

	// Allocation for tenant 1
	al1 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	// Instance Type 1 - Main test instance type
	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	// Add InfiniBand capability to Instance Type 1 for InfiniBand interface tests
	common.TestBuildMachineCapability(t, dbSession, nil, &ist1.ID, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-6]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(3), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	common.TestBuildMachineCapability(t, dbSession, nil, &ist1.ID, cdbm.MachineCapabilityTypeNetwork, "MT42822 BlueField-2 integrated ConnectX-6 Dx network controller", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU), nil)

	// Allocation constraint for ist1 with quota of 15
	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 15, ipu)
	assert.NotNil(t, alc1)

	// Use dedicated allocation inventory for VPC-selection coverage so the
	// request cannot consume capacity needed by unrelated table cases.
	istVpcSelection := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-vpc-selection", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, istVpcSelection)
	alcVpcSelection := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, istVpcSelection.ID, cdbm.AllocationConstraintTypeReserved, 2, ipu)
	assert.NotNil(t, alcVpcSelection)
	for i := 0; i < 2; i++ {
		mc := testBatchBuildMachineWithNVLinkDomain(t, dbSession, ip.ID, st1.ID, "nvlink-domain-vpc-selection")
		testInstanceBuildMachineInstanceType(t, dbSession, mc, istVpcSelection)
	}

	// Create 15 machines for ist1 (all on same NVLink domain for topology optimization test)
	for i := 0; i < 15; i++ {
		mc := testBatchBuildMachineWithNVLinkDomain(t, dbSession, ip.ID, st1.ID, "nvlink-domain-1")
		testInstanceBuildMachineInstanceType(t, dbSession, mc, ist1)
	}

	// Operating System
	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-os-1", tn1, cdbm.OperatingSystemTypeImage, true, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	ossa1 := testInstanceBuildOperatingSystemSiteAssociation(t, dbSession, st1.ID, os1.ID)
	assert.NotNil(t, ossa1)

	// Operating System without site association (for testing OS not in VPC site)
	osNoSiteAssoc := testInstanceBuildOperatingSystem(t, dbSession, "test-os-no-site-assoc", tn1, cdbm.OperatingSystemTypeImage, true, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, osNoSiteAssoc)

	// VPC and Subnet
	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	ipbVpcPrefix := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb-vpcprefix", st1, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "10.0.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
	vpcFNN := testInstanceBuildVPC(t, dbSession, "test-vpc-fnn-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
	vpcPrefix1 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-1", st1, tn1, vpcFNN.ID, &ipbVpcPrefix.ID, cutil.GetPtr("10.0.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	ipbVpcPrefixSecondary := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb-vpcprefix-secondary", st1, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "10.1.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
	vpcFNNSecondary := testInstanceBuildVPC(t, dbSession, "test-vpc-fnn-2", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
	vpcPrefixSecondary := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-secondary", st1, tn1, vpcFNNSecondary.ID, &ipbVpcPrefixSecondary.ID, cutil.GetPtr("10.1.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	ipbVpcPrefixSite2 := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb-vpcprefix-site2", st2, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "10.2.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
	vpcFNNSite2 := testInstanceBuildVPC(t, dbSession, "test-vpc-fnn-site-2", ip, tn1, st2, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
	vpcPrefixSite2 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-site2", st2, tn1, vpcFNNSite2.ID, &ipbVpcPrefixSite2.ID, cutil.GetPtr("10.2.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	assert.NotNil(t, vpcPrefixSite2)

	// InfiniBand Partition for testing InfiniBand Interfaces
	ibp1 := testBuildIBPartition(t, dbSession, "test-ibp-1", tnOrg, st1, tn1, cutil.GetPtr(uuid.New()), cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
	assert.NotNil(t, ibp1)

	// NVLink Logical Partition for testing NVLink Interfaces
	nvllp1 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-1", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg, st1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp1)

	// Add NVLink GPU capability to Instance Type 1 for NVLink interface tests
	mcNvlType := common.TestBuildMachineCapability(t, dbSession, nil, &ist1.ID, cdbm.MachineCapabilityTypeGPU, "NVIDIA GB200", nil, nil, cutil.GetPtr("NVIDIA"), cutil.GetPtr(4), (*cdbm.MachineCapabilityDeviceType)(cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeNVLink)), nil)
	assert.NotNil(t, mcNvlType)

	// DPU Extension Service for testing DPU Extension Service Deployments
	desVersion1 := "V1-T1761856992374052"
	desVersion2 := "V2-T1761856992374052"
	des1 := common.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-1", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, desVersion1, cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des1)
	// Allow multiple active versions for regression/positive tests.
	des1.ActiveVersions = []string{desVersion1, desVersion2}
	_, err := dbSession.DB.NewUpdate().Model(des1).Column("active_versions").Where("id = ?", des1.ID).Exec(context.Background())
	assert.NoError(t, err)

	// SSHKeyGroup for testing SSH Key Group IDs
	skg1 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-1", tnOrg, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg1)
	skgsa1 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st1.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa1)

	// Instance Type 2 - For quota test with limit of 5
	ist2 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-2", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist2)
	alc2 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist2.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc2)
	for i := 0; i < 10; i++ {
		mc := testBatchBuildMachineWithNVLinkDomain(t, dbSession, ip.ID, st1.ID, "nvlink-domain-1")
		testInstanceBuildMachineInstanceType(t, dbSession, mc, ist2)
	}

	// Instance Type 3 - For insufficient machines test (only 2 machines)
	ist3 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-3", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist3)
	alc3 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist3.ID, cdbm.AllocationConstraintTypeReserved, 10, ipu)
	assert.NotNil(t, alc3)
	// Only 2 machines, each on different NVLink domain (to test insufficient machines with topology optimization)
	mc3a := testBatchBuildMachineWithNVLinkDomain(t, dbSession, ip.ID, st1.ID, "nvlink-domain-a")
	testInstanceBuildMachineInstanceType(t, dbSession, mc3a, ist3)
	mc3b := testBatchBuildMachineWithNVLinkDomain(t, dbSession, ip.ID, st1.ID, "nvlink-domain-b")
	testInstanceBuildMachineInstanceType(t, dbSession, mc3b, ist3)

	// Site 2 - Not ready site (for status check test)
	stNotReady := testInstanceBuildSite(t, dbSession, ip, "test-site-not-ready", cdbm.SiteStatusPending, true, ipu)
	assert.NotNil(t, stNotReady)
	tsNotReady := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, stNotReady.ID, tnu1.ID)
	assert.NotNil(t, tsNotReady)
	alNotReady := testInstanceSiteBuildAllocation(t, dbSession, stNotReady, tn1, "test-allocation-not-ready", ipu)
	assert.NotNil(t, alNotReady)
	ist4 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-4", stNotReady, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist4)
	alc4 := testInstanceSiteBuildAllocationContraints(t, dbSession, alNotReady, cdbm.AllocationResourceTypeInstanceType, ist4.ID, cdbm.AllocationConstraintTypeReserved, 10, ipu)
	assert.NotNil(t, alc4)
	for i := 0; i < 5; i++ {
		mc := testBatchBuildMachineWithNVLinkDomain(t, dbSession, ip.ID, stNotReady.ID, "nvlink-domain-1")
		testInstanceBuildMachineInstanceType(t, dbSession, mc, ist4)
	}
	vpcNotReadySite := testInstanceBuildVPC(t, dbSession, "test-vpc-not-ready-site", ip, tn1, stNotReady, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	subnetNotReadySite := testInstanceBuildSubnet(t, dbSession, "test-subnet-not-ready-site", tn1, vpcNotReadySite, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)

	// VPC not ready (for VPC status check test)
	vpcNotReady := testInstanceBuildVPC(t, dbSession, "test-vpc-not-ready", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu1)
	subnetVpcNotReady := testInstanceBuildSubnet(t, dbSession, "test-subnet-vpc-not-ready", tn1, vpcNotReady, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)

	// Subnet not ready (for Subnet status check test)
	subnetNotReady := testInstanceBuildSubnet(t, dbSession, "test-subnet-not-ready", tn1, vpc1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusPending, tnu1)

	// DPU Extension Service on different Site (for DPU site mismatch test)
	des2 := common.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-2", model.DpuExtensionServiceTypeKubernetesPod, tn1, stNotReady, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des2)

	// Tenant 2 for testing OS not owned by tenant
	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles2 := []string{authz.TenantAdminRole}
	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles2)
	tn2 := testInstanceBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, tnu2)
	vpcOtherTenant := testInstanceBuildVPC(t, dbSession, "test-vpc-other-tenant", ip, tn2, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu2)
	subnetOtherTenant := testInstanceBuildSubnet(t, dbSession, "test-subnet-other-tenant", tn2, vpcOtherTenant, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu2)

	// OS owned by different tenant (for OS ownership test)
	osOtherTenant := testInstanceBuildOperatingSystem(t, dbSession, "test-os-other-tenant", tn2, cdbm.OperatingSystemTypeIPXE, false, nil, false, cdbm.OperatingSystemStatusReady, tnu2)
	assert.NotNil(t, osOtherTenant)

	// Setup Echo and config
	e := echo.New()
	cfg := common.GetTestConfig()

	// Setup Temporal mocks
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	// Mock workflow run
	wid := "test-batch-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)
	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	// Mock site Temporal client
	tsc := &tmocks.Client{}
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(wrun, nil)
	scp.IDClientMap[st1.ID.String()] = tsc
	scp.IDClientMap[stNotReady.ID.String()] = tsc // For site not ready test

	tc := &tmocks.Client{}

	// Helper function to create shared interface configuration for batch requests
	// All instances in the batch share the same interface configuration
	createInterfacesForCount := func(count int, subnetID string) []model.APIInterfaceCreateOrUpdateRequest {
		// Note: count parameter is kept for backward compatibility but not used
		// since interfaces are now shared across all instances
		return []model.APIInterfaceCreateOrUpdateRequest{
			{SubnetID: cutil.GetPtr(subnetID)},
		}
	}

	// Test cases
	type fields struct {
		dbSession *cdb.Session
		tc        *tmocks.Client
		scp       *sc.ClientPool
		cfg       *config.Config
	}

	type args struct {
		reqData  *model.APIBatchInstanceCreateRequest
		reqOrg   string
		reqUser  *cdbm.User
		respCode int
		respMsg  string
	}

	tests := []struct {
		name                     string
		fields                   fields
		args                     args
		expectedControllerVpcIDs map[string]uuid.UUID
		wantErr                  bool
	}{
		{
			name: "test batch instance create API endpoint succeeds with valid request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-success",
					Count:          3,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(3, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint rejects Machine label selector when tenant is not authorized",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:           "test-batch-unauthorized-machine-label-filters",
					Count:                2,
					TenantID:             tn2.ID.String(),
					InstanceTypeID:       ist1.ID.String(),
					VpcID:                vpcOtherTenant.ID.String(),
					Interfaces:           createInterfacesForCount(2, subnetOtherTenant.ID.String()),
					IpxeScript:           cutil.GetPtr("test script"),
					MachineLabelSelector: map[string]string{"failure-domain": "fd-a"},
				},
				reqOrg:   tnOrg2,
				reqUser:  tnu2,
				respCode: http.StatusForbidden,
				respMsg:  "Tenant does not have capability to create Instances using Machine label selector",
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint preserves IPv6-only and dual-stack VPC selection intent",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:      "test-batch-vpc-selection",
					Count:           2,
					TenantID:        tn1.ID.String(),
					InstanceTypeID:  istVpcSelection.ID.String(),
					VpcID:           vpcFNN.ID.String(),
					SecondaryVpcIDs: []string{vpcFNNSecondary.ID.String()},
					IpxeScript:      cutil.GetPtr("test script"),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcID:      cutil.GetPtr(vpcFNN.ID.String()),
							IPFamilies: []model.IPFamily{model.IPFamilyIPv6},
							IsPhysical: true,
						},
						{
							VpcID:      cutil.GetPtr(vpcFNNSecondary.ID.String()),
							IPFamilies: []model.IPFamily{model.IPFamilyIPv4, model.IPFamilyIPv6},
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			expectedControllerVpcIDs: map[string]uuid.UUID{
				vpcFNN.ID.String():          *vpcFNN.ControllerVpcID,
				vpcFNNSecondary.ID.String(): *vpcFNNSecondary.ControllerVpcID,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with missing namePrefix",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					Count:          3,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(3, subnet1.ID.String()),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with count zero",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-zero-count",
					Count:          0,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(3, subnet1.ID.String()),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with count exceeds maximum (73 > 72)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-max-count",
					Count:          73,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(3, subnet1.ID.String()),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint succeeds with same prefix (random suffix avoids conflict)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-success", // Same prefix but random suffix ensures unique names
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(2, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint rejects requested IP on interfaces",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-vpcprefix-ip",
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpcFNN.ID.String(),
					IpxeScript:     cutil.GetPtr("test script"),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID: cutil.GetPtr(vpcPrefix1.ID.String()),
							IPAddress:   cutil.GetPtr("10.0.0.11"),
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  "batch instance create does not support `ipAddress` on interfaces",
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint succeeds with secondary VPC Prefix interface",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:      "test-batch-secondary-vpc",
					Count:           2,
					TenantID:        tn1.ID.String(),
					InstanceTypeID:  ist1.ID.String(),
					VpcID:           vpcFNN.ID.String(),
					SecondaryVpcIDs: []string{vpcFNNSecondary.ID.String()},
					IpxeScript:      cutil.GetPtr("test script"),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix1.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
							InlineRoutingProfile: &model.APIInterfaceInlineRoutingProfile{
								AllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
							},
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSecondary.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails when requested secondary VPCs do not match interface VPCs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:      "test-batch-secondary-vpc-mismatch",
					Count:           2,
					TenantID:        tn1.ID.String(),
					InstanceTypeID:  ist1.ID.String(),
					VpcID:           vpcFNN.ID.String(),
					SecondaryVpcIDs: []string{vpcFNNSecondary.ID.String()},
					IpxeScript:      cutil.GetPtr("test script"),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix1.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  "One or more Interfaces in request data specify VPC Prefixes that do not belong to VPCs specified in `vpcId` or `secondaryVpcIds`",
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails when an interface uses a VPC outside requested VPC IDs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-unexpected-vpc",
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpcFNN.ID.String(),
					IpxeScript:     cutil.GetPtr("test script"),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix1.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSecondary.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  fmt.Sprintf("One or more Interfaces specify VPC Prefix: %s belonging to VPC: %s which is not specified in 'vpcId' or 'secondaryVpcIds'", vpcPrefixSecondary.ID.String(), vpcFNNSecondary.ID.String()),
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails when primary physical interface uses a prefix from a secondary VPC",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:      "test-batch-primary-must-match-vpc",
					Count:           2,
					TenantID:        tn1.ID.String(),
					InstanceTypeID:  ist1.ID.String(),
					VpcID:           vpcFNN.ID.String(),
					SecondaryVpcIDs: []string{vpcFNNSecondary.ID.String()},
					IpxeScript:      cutil.GetPtr("test script"),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSecondary.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  "The primary physical Interface for deviceInstance: 0 must use a VPC Prefix that belongs to VPC specified in `vpcId`",
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint preserves prefix response for mixed selectors without device info",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:      "test-batch-primary-must-match-vpc-no-device",
					Count:           2,
					TenantID:        tn1.ID.String(),
					InstanceTypeID:  ist1.ID.String(),
					VpcID:           vpcFNN.ID.String(),
					SecondaryVpcIDs: []string{vpcFNNSecondary.ID.String()},
					IpxeScript:      cutil.GetPtr("test script"),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID: cutil.GetPtr(vpcPrefixSecondary.ID.String()),
							IsPhysical:  true,
						},
						{
							VpcID:      cutil.GetPtr(vpcFNN.ID.String()),
							IPFamilies: []model.IPFamily{model.IPFamilyIPv4},
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  "The primary physical Interface must use a VPC Prefix that belongs to VPC specified in `vpcId`",
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails when primary physical interface uses a VPC Prefix from another Site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-prefix-wrong-site-primary",
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpcFNN.ID.String(),
					IpxeScript:     cutil.GetPtr("test script"),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSite2.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  fmt.Sprintf("VPC Prefix: %s specified in request does not belong to Site", vpcPrefixSite2.ID.String()),
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails when secondary interface uses a VPC Prefix from another Site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:      "test-batch-prefix-wrong-site-secondary",
					Count:           2,
					TenantID:        tn1.ID.String(),
					InstanceTypeID:  ist1.ID.String(),
					VpcID:           vpcFNN.ID.String(),
					SecondaryVpcIDs: []string{vpcFNNSecondary.ID.String()},
					IpxeScript:      cutil.GetPtr("test script"),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix1.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSite2.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  fmt.Sprintf("VPC Prefix: %s specified in request does not belong to Site", vpcPrefixSite2.ID.String()),
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with quota exceeded",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-quota-1",
					Count:          3,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist2.ID.String(), // ist2 has quota limit of 5
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(3, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated, // First request should succeed
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with insufficient machines",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-insufficient",
					Count:          5, // Only 2 machines available for ist3
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist3.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(5, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusConflict,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with invalid TenantID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-invalid-tenant",
					Count:          3,
					TenantID:       uuid.New().String(), // Non-existent tenant
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(3, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with invalid InstanceTypeID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-invalid-instance-type",
					Count:          3,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: uuid.New().String(), // Non-existent instance type
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(3, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with invalid VpcID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-invalid-vpc",
					Count:          3,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          uuid.New().String(), // Non-existent VPC
					Interfaces:     createInterfacesForCount(3, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with Site not ready",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-site-not-ready",
					Count:          3,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist4.ID.String(), // ist4 is on stNotReady
					VpcID:          vpcNotReadySite.ID.String(),
					Interfaces:     createInterfacesForCount(3, subnetNotReadySite.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with VPC not ready",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-vpc-not-ready",
					Count:          3,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpcNotReady.ID.String(),
					Interfaces:     createInterfacesForCount(3, subnetVpcNotReady.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with Subnet not ready",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-subnet-not-ready",
					Count:          3,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(3, subnetNotReady.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		// SSHKeyGroup test - covering SSH Key Group creation branch
		{
			name: "test batch instance create API endpoint succeeds with SSHKeyGroupIDs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-with-sshkey",
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(2, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
					SSHKeyGroupIDs: []string{skg1.ID.String()},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			wantErr: false,
		},
		// DPU Extension Service Deployments test - covering DPU deployment creation branch
		{
			name: "test batch instance create API endpoint succeeds with DPU Extension Service Deployments",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-with-dpu",
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(2, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: des1.ID.String(),
							Version:               desVersion1,
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint succeeds with multiple DPU deployment versions for same service",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix: "test-batch-with-dpu-multi-version",
					Count:      2,
					TenantID:   tn1.ID.String(),
					// Use ist2 to avoid consuming ist1 quota needed by later OS tests.
					InstanceTypeID: ist2.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(2, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: des1.ID.String(),
							Version:               desVersion1,
						},
						{
							DpuExtensionServiceID: des1.ID.String(),
							Version:               desVersion2,
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			wantErr: false,
		},
		// DPU Extension Service Deployments regression test:
		// same DPU Extension Service ID with multiple different versions is allowed by API model validation,
		// and each (id, version) request must be validated against ActiveVersions. This catches the previous
		// handler bug where a map[serviceID]version could overwrite and skip validation for earlier entries.
		{
			name: "test batch instance create API endpoint fails when one of multiple DPU deployments for same service has invalid version",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-dpu-multi-version-invalid",
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(2, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: des1.ID.String(),
							Version:               "INVALID-VERSION",
						},
						{
							DpuExtensionServiceID: des1.ID.String(),
							Version:               "V1-T1761856992374052",
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  "Version:",
			},
			wantErr: false,
		},
		// NVLink Interfaces test - covering NVLink interface creation branch
		{
			name: "test batch instance create API endpoint succeeds with NVLink Interfaces",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-with-nvlink",
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(2, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{
							DeviceInstance:           0,
							NVLinkLogicalPartitionID: nvllp1.ID.String(),
						},
						{
							DeviceInstance:           1,
							NVLinkLogicalPartitionID: nvllp1.ID.String(),
						},
						{
							DeviceInstance:           2,
							NVLinkLogicalPartitionID: nvllp1.ID.String(),
						},
						{
							DeviceInstance:           3,
							NVLinkLogicalPartitionID: nvllp1.ID.String(),
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			wantErr: false,
		},
		// InfiniBand Interfaces test - covering InfiniBand interface creation branch
		{
			name: "test batch instance create API endpoint succeeds with InfiniBand Interfaces",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-with-ib",
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(2, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp1.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			wantErr: false,
		},
		// DPU Extension Service failure tests
		{
			name: "test batch instance create API endpoint fails with non-existing DPU Extension Service ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-dpu-not-found",
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(2, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: uuid.New().String(),
							Version:               "V1-T1761856992374052",
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  "Could not find DPU Extension Service",
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with DPU Extension Service from different Site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-batch-dpu-wrong-site",
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					Interfaces:     createInterfacesForCount(2, subnet1.ID.String()),
					IpxeScript:     cutil.GetPtr("test script"),
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: des2.ID.String(),
							Version:               "V1-T1761856992374052",
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusForbidden,
				respMsg:  "does not belong to Site where Instances are being created",
			},
			wantErr: false,
		},
		// OperatingSystemID tests - covering buildBatchInstanceCreateRequestOsConfig OS branch
		{
			name: "test batch instance create API endpoint with OperatingSystemID (Image based OS temporarily expect StatusBadRequest)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:        "test-batch-with-os",
					Count:             2,
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    ist1.ID.String(),
					VpcID:             vpc1.ID.String(),
					Interfaces:        createInterfacesForCount(2, subnet1.ID.String()),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  "Creation of Instance with Image based Operating System is not supported. Site must have ImageBasedOperatingSystem capability enabled.",
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with invalid OperatingSystemID UUID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:        "test-batch-invalid-os-uuid",
					Count:             2,
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    ist1.ID.String(),
					VpcID:             vpc1.ID.String(),
					Interfaces:        createInterfacesForCount(2, subnet1.ID.String()),
					OperatingSystemID: cutil.GetPtr("not-a-valid-uuid"),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  `"operatingSystemId":"must be a valid UUID"`,
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with OperatingSystem not in VPC site (Image based OS: expect not supported)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:        "test-batch-os-wrong-site",
					Count:             2,
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    ist1.ID.String(),
					VpcID:             vpc1.ID.String(),
					Interfaces:        createInterfacesForCount(2, subnet1.ID.String()),
					OperatingSystemID: cutil.GetPtr(osNoSiteAssoc.ID.String()),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  "Creation of Instance with Image based Operating System is not supported. Site must have ImageBasedOperatingSystem capability enabled.",
			},
			wantErr: false,
		},
		{
			name: "test batch instance create API endpoint fails with OperatingSystem not owned by Tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:        "test-batch-os-wrong-tenant",
					Count:             2,
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    ist1.ID.String(),
					VpcID:             vpc1.ID.String(),
					Interfaces:        createInterfacesForCount(2, subnet1.ID.String()),
					OperatingSystemID: cutil.GetPtr(osOtherTenant.ID.String()),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  "OperatingSystem specified in request is not owned by Tenant",
			},
			wantErr: false,
		},
		{
			// vpc1 is an ETHERNET_VIRTUALIZER VPC; `auto: true` is only
			// valid for instances in a Flat VPC. The handler-side
			// cross-check should reject the mismatch before any workflow
			// is invoked.
			name: "test batch instance create API endpoint rejects auto=true on a non-Flat VPC",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIBatchInstanceCreateRequest{
					NamePrefix:     "test-auto-non-flat",
					Count:          2,
					TenantID:       tn1.ID.String(),
					InstanceTypeID: ist1.ID.String(),
					VpcID:          vpc1.ID.String(),
					IpxeScript:     cutil.GetPtr("test script"),
					AutoNetwork:    true,
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMsg:  "`autoNetwork` is only supported when the VPC has `networkVirtualizationType` set to `FLAT`",
			},
			wantErr: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			bcih := BatchCreateInstanceHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       tt.fields.scp,
				cfg:       tt.fields.cfg,
			}

			jsonData, _ := json.Marshal(tt.args.reqData)

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(jsonData)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.reqOrg)
			ec.Set("user", tt.args.reqUser)

			err := bcih.Handle(ec)
			if tt.wantErr {
				assert.NotNil(t, err)
			} else {
				// Check response code
				assert.Equal(t, tt.args.respCode, rec.Code, "Response body: %s", rec.Body.String())
				if tt.args.respMsg != "" {
					assert.Contains(t, rec.Body.String(), tt.args.respMsg)
				}

				// For successful creations, verify response structure
				if rec.Code == http.StatusCreated {
					var response []model.APIInstance
					jsonErr := json.Unmarshal(rec.Body.Bytes(), &response)
					assert.Nil(t, jsonErr)
					assert.Equal(t, tt.args.reqData.Count, len(response), "Expected %d instances, got %d", tt.args.reqData.Count, len(response))

					hasInlineRoutingProfile := false
					for _, reqIfc := range tt.args.reqData.Interfaces {
						if reqIfc.InlineRoutingProfile != nil {
							hasInlineRoutingProfile = true
							break
						}
					}

					// Verify instance names follow the pattern: namePrefix-randomSuffix-index
					for i, inst := range response {
						expectedPrefix := tt.args.reqData.NamePrefix + "-"
						expectedSuffix := fmt.Sprintf("-%d", i+1)
						assert.True(t, strings.HasPrefix(inst.Name, expectedPrefix), "Instance %d name should start with %s, got %s", i, expectedPrefix, inst.Name)
						assert.True(t, strings.HasSuffix(inst.Name, expectedSuffix), "Instance %d name should end with %s, got %s", i, expectedSuffix, inst.Name)
						assert.ElementsMatch(t, tt.args.reqData.SecondaryVpcIDs, inst.SecondaryVpcIDs)

						if len(tt.expectedControllerVpcIDs) > 0 {
							require.Len(t, inst.Interfaces, len(tt.args.reqData.Interfaces))
							for j, reqIfc := range tt.args.reqData.Interfaces {
								assert.Equal(t, reqIfc.VpcID, inst.Interfaces[j].VpcID)
								assert.Equal(t, reqIfc.IPFamilies, inst.Interfaces[j].IPFamilies)
								assert.Nil(t, inst.Interfaces[j].VpcPrefixID)
							}

							ifcDAO := cdbm.NewInterfaceDAO(dbSession)
							dbIfcs, _, ierr := ifcDAO.GetAll(ec.Request().Context(), nil,
								cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{uuid.MustParse(inst.ID)}},
								cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InterfaceOrderByCreated, Order: cdbp.OrderAscending}},
								nil)
							require.NoError(t, ierr)
							require.Len(t, dbIfcs, len(tt.args.reqData.Interfaces))
							for j, reqIfc := range tt.args.reqData.Interfaces {
								require.NotNil(t, dbIfcs[j].VpcID)
								assert.Equal(t, *reqIfc.VpcID, dbIfcs[j].VpcID.String())
								require.NotNil(t, dbIfcs[j].VpcIPFamilyMode)
								assert.Equal(t, reqIfc.VpcIPFamilyMode(), *dbIfcs[j].VpcIPFamilyMode)
								assert.Nil(t, dbIfcs[j].VpcPrefixID)
							}
						}

						if hasInlineRoutingProfile {
							require.Len(t, inst.Interfaces, len(tt.args.reqData.Interfaces))
							for j, reqIfc := range tt.args.reqData.Interfaces {
								if reqIfc.InlineRoutingProfile != nil {
									require.NotNil(t, inst.Interfaces[j].InlineRoutingProfile)
									assert.Equal(t, reqIfc.InlineRoutingProfile.AllowedAnycastPrefixes, inst.Interfaces[j].InlineRoutingProfile.AllowedAnycastPrefixes)
								}
							}

							ifcDAO := cdbm.NewInterfaceDAO(dbSession)
							dbIfcs, _, ierr := ifcDAO.GetAll(ec.Request().Context(), nil,
								cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{uuid.MustParse(inst.ID)}},
								cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InterfaceOrderByCreated, Order: cdbp.OrderAscending}},
								nil)
							require.NoError(t, ierr)
							require.Len(t, dbIfcs, len(tt.args.reqData.Interfaces))
							for j, reqIfc := range tt.args.reqData.Interfaces {
								if reqIfc.InlineRoutingProfile != nil {
									require.NotNil(t, dbIfcs[j].InlineRoutingProfile)
									assert.Equal(t, reqIfc.InlineRoutingProfile.AllowedAnycastPrefixes, dbIfcs[j].InlineRoutingProfile.AllowedAnycastPrefixes)
								}
							}
						}
					}

					if hasInlineRoutingProfile || len(tt.expectedControllerVpcIDs) > 0 {
						var batchReq *corev1.BatchInstanceAllocationRequest
						for i := len(tsc.Calls) - 1; i >= 0; i-- {
							call := tsc.Calls[i]
							if call.Method == "ExecuteWorkflow" && len(call.Arguments) > 3 && call.Arguments[2] == "CreateInstances" {
								batchReq = call.Arguments[3].(*corev1.BatchInstanceAllocationRequest)
								break
							}
						}
						require.NotNil(t, batchReq)
						require.Len(t, batchReq.InstanceRequests, len(response))
						for _, instReq := range batchReq.InstanceRequests {
							require.Len(t, instReq.Config.Network.Interfaces, len(tt.args.reqData.Interfaces))
							for j, reqIfc := range tt.args.reqData.Interfaces {
								if reqIfc.InlineRoutingProfile != nil {
									assertInterfaceRoutingProfilePrefixes(t, instReq.Config.Network.Interfaces[j].RoutingProfile, reqIfc.InlineRoutingProfile.AllowedAnycastPrefixes)
								}
								if reqIfc.VpcID != nil {
									expectedControllerVpcID, ok := tt.expectedControllerVpcIDs[*reqIfc.VpcID]
									require.True(t, ok)
									assertInterfaceVpcSelection(t, instReq.Config.Network.Interfaces[j], expectedControllerVpcID, reqIfc.VpcIPFamilyMode())
								}
							}
						}
					}
				}
			}
		})
	}

	// Additional test: quota exceeded on second request
	t.Run("test batch instance create API endpoint fails with quota exceeded on second request", func(t *testing.T) {
		bcih := BatchCreateInstanceHandler{
			dbSession: dbSession,
			tc:        tc,
			scp:       scp,
			cfg:       cfg,
		}

		// Second request should fail due to quota (3 existing + 3 new = 6 > 5 limit)
		reqData := &model.APIBatchInstanceCreateRequest{
			NamePrefix:     "test-quota-2",
			Count:          3,
			TenantID:       tn1.ID.String(),
			InstanceTypeID: ist2.ID.String(),
			VpcID:          vpc1.ID.String(),
			Interfaces:     createInterfacesForCount(3, subnet1.ID.String()),
			IpxeScript:     cutil.GetPtr("test script"),
		}

		jsonData, _ := json.Marshal(reqData)
		req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(jsonData)))
		req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		rec := httptest.NewRecorder()

		ec := e.NewContext(req, rec)
		ec.SetParamNames("orgName")
		ec.SetParamValues(tnOrg)
		ec.Set("user", tnu1)

		_ = bcih.Handle(ec)
		assert.Equal(t, http.StatusForbidden, rec.Code, "Expected quota exceeded error, got: %s", rec.Body.String())
	})

}
