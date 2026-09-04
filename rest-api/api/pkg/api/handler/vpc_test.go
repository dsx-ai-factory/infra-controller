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
	"reflect"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	sutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	"go.temporal.io/api/enums/v1"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/proto"

	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	oteltrace "go.opentelemetry.io/otel/trace"
)

func testVPCInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

func TestPowerResourceGroupAPIError(t *testing.T) {
	tests := []struct {
		name        string
		err         error
		fallback    string
		wantCode    int
		wantMessage string
	}{
		{
			name:        "maps DPS name collision to conflict",
			err:         fmt.Errorf("create replacement group: %w", dpsclient.ErrResourceGroupAlreadyExists),
			fallback:    "Failed to change DPS resource group",
			wantCode:    http.StatusConflict,
			wantMessage: "Power resource group already exists",
		},
		{
			name:        "preserves generic DPS failure mapping",
			err:         errors.New("DPS unavailable"),
			fallback:    "Failed to create DPS resource group",
			wantCode:    http.StatusServiceUnavailable,
			wantMessage: "Failed to create DPS resource group",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			apiErr := powerResourceGroupAPIError(test.err, test.fallback)
			assert.Equal(t, test.wantCode, apiErr.Code)
			assert.Equal(t, test.wantMessage, apiErr.Message)
		})
	}
}

// testVPCSetupSchema resets the tables required by VPC handler and
// privilege-resolution tests.
func testVPCSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	// create Tenant Account table used for effective per-Site privilege resolution
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantAccount)(nil))
	assert.Nil(t, err)
	// create Tenant Site table used for per-Site privilege overrides
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantSite)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil))
	assert.Nil(t, err)
	// create VPC Prefix table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.VpcPrefix)(nil))
	assert.Nil(t, err)
	// create IP Block table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IPBlock)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Subnet)(nil))
	assert.Nil(t, err)
	// create Status Details table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create NVLinkLogicalPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkLogicalPartition)(nil))
	assert.Nil(t, err)
	// create VPC table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Vpc)(nil))
	assert.Nil(t, err)
}

func testVPCSiteBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
	ipDAO := cdbm.NewInfrastructureProviderDAO(dbSession)

	ip, err := ipDAO.Create(context.Background(), nil, cdbm.InfrastructureProviderCreateInput{
		Name:        name,
		DisplayName: cutil.GetPtr("Test Infrastructure Provider"),
		Org:         org,
		CreatedBy:   user.ID,
	})
	assert.Nil(t, err)

	return ip
}

func testVPCBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, isNativeNetworkingEnabled bool, isNVLinkPartitionEnabled bool, status string, user *cdbm.User) *cdbm.Site {
	stDAO := cdbm.NewSiteDAO(dbSession)

	st, err := stDAO.Create(context.Background(), nil, cdbm.SiteCreateInput{
		Name:                          name,
		DisplayName:                   cutil.GetPtr("Test Site"),
		Description:                   cutil.GetPtr("Test Site Description"),
		Org:                           ip.Org,
		InfrastructureProviderID:      ip.ID,
		SiteControllerVersion:         cutil.GetPtr("1.0.0"),
		SiteAgentVersion:              cutil.GetPtr("1.0.0"),
		RegistrationToken:             cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration:   cutil.GetPtr(cdb.GetCurTime()),
		IsInfinityEnabled:             false,
		Config:                        cdbm.SiteConfig{NativeNetworking: isNativeNetworkingEnabled, NVLinkPartition: isNVLinkPartitionEnabled},
		SerialConsoleHostname:         cutil.GetPtr("TestSshHostname"),
		IsSerialConsoleEnabled:        true,
		SerialConsoleIdleTimeout:      cutil.GetPtr(30),
		SerialConsoleMaxSessionLength: cutil.GetPtr(60),
		Status:                        status,
		CreatedBy:                     user.ID,
	})
	assert.Nil(t, err)

	return st
}

func testVPCBuildTenant(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.Tenant {
	tnDAO := cdbm.NewTenantDAO(dbSession)

	tn, err := tnDAO.Create(context.Background(), nil, cdbm.TenantCreateInput{
		Name:        name,
		DisplayName: cutil.GetPtr("Test Tenant"),
		Org:         org,
		CreatedBy:   user.ID,
	})
	assert.Nil(t, err)

	return tn
}

func testVPCBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, org string, roles []string) *cdbm.User {
	uDAO := cdbm.NewUserDAO(dbSession)

	u, err := uDAO.Create(
		context.Background(),
		nil,
		cdbm.UserCreateInput{
			AuxiliaryID: nil,
			StarfleetID: &starfleetID,
			Email:       cutil.GetPtr("jdoe@test.com"),
			FirstName:   cutil.GetPtr("John"),
			LastName:    cutil.GetPtr("Doe"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       roles,
				},
			},
		},
	)
	assert.Nil(t, err)

	return u
}

func testVPCSiteBuildAllocation(t *testing.T, dbSession *cdb.Session, st *cdbm.Site, tn *cdbm.Tenant, name string, user *cdbm.User) *cdbm.Allocation {
	alDAO := cdbm.NewAllocationDAO(dbSession)

	createInput := cdbm.AllocationCreateInput{
		Name:                     name,
		Description:              cutil.GetPtr("Test Allocation Description"),
		InfrastructureProviderID: st.InfrastructureProviderID,
		TenantID:                 tn.ID,
		SiteID:                   st.ID,
		Status:                   cdbm.AllocationStatusPending,
		CreatedBy:                user.ID,
	}
	al, err := alDAO.Create(context.Background(), nil, createInput)
	assert.Nil(t, err)

	return al
}

func testVPCBuildVPC(t *testing.T, dbSession *cdb.Session, name string, ip *cdbm.InfrastructureProvider, tn *cdbm.Tenant, st *cdbm.Site, nvt *string, defaultNVLinkLogicalPartitionID *uuid.UUID, labels map[string]string, status string, user *cdbm.User) *cdbm.Vpc {
	vpcDAO := cdbm.NewVpcDAO(dbSession)

	input := cdbm.VpcCreateInput{
		Name:                      name,
		Description:               cutil.GetPtr("Test Vpc"),
		Org:                       tn.Org,
		InfrastructureProviderID:  ip.ID,
		TenantID:                  tn.ID,
		SiteID:                    st.ID,
		NetworkVirtualizationType: nvt,
		NVLinkLogicalPartitionID:  defaultNVLinkLogicalPartitionID,
		ControllerVpcID:           cutil.GetPtr(uuid.New()),
		Labels:                    labels,
		Status:                    status,
		CreatedBy:                 *user,
	}

	vpc, err := vpcDAO.Create(context.Background(), nil, input)
	assert.Nil(t, err)

	return vpc
}

func testUpdateVPC(t *testing.T, dbSession *cdb.Session, vpc *cdbm.Vpc) *cdbm.Vpc {
	_, err := dbSession.DB.NewUpdate().Where("id = ?", vpc.ID).Model(vpc).Exec(context.Background())
	assert.Nil(t, err)
	return vpc
}

func testVPCBuildSubnet(t *testing.T, dbSession *cdb.Session, name string, tn *cdbm.Tenant, vpc *cdbm.Vpc, user *cdbm.User) *cdbm.Subnet {
	subnetDAO := cdbm.NewSubnetDAO(dbSession)

	subnet, err := subnetDAO.Create(context.Background(), nil, cdbm.SubnetCreateInput{
		Name:         name,
		Description:  cutil.GetPtr("Test Subnet"),
		Org:          tn.Org,
		SiteID:       vpc.SiteID,
		VpcID:        vpc.ID,
		TenantID:     tn.ID,
		PrefixLength: 0,
		Status:       cdbm.SubnetStatusPending,
		CreatedBy:    user.ID,
	})
	assert.Nil(t, err)

	return subnet
}

func testVPCBuildVPCPrefix(t *testing.T, dbSession *cdb.Session, name string, tn *cdbm.Tenant, vpc *cdbm.Vpc, ipbID *uuid.UUID, prefix string, user *cdbm.User) *cdbm.VpcPrefix {
	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)

	vpcPrefix, err := vpcPrefixDAO.Create(context.Background(), nil, cdbm.VpcPrefixCreateInput{
		Name:         name,
		SiteID:       vpc.SiteID,
		VpcID:        vpc.ID,
		TenantID:     tn.ID,
		IpBlockID:    ipbID,
		Prefix:       prefix,
		PrefixLength: 24,
		Status:       cdbm.VpcPrefixStatusReady,
		CreatedBy:    user.ID,
	})
	assert.Nil(t, err)

	return vpcPrefix
}

func testVPCBuildNVLinkLogicalPartition(t *testing.T, dbSession *cdb.Session, name string, description *string, org string, site *cdbm.Site, tenant *cdbm.Tenant, status cdbm.NVLinkLogicalPartitionStatus, user *cdbm.User) *cdbm.NVLinkLogicalPartition {
	nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(dbSession)

	nvllp, err := nvllpDAO.Create(context.Background(), nil, cdbm.NVLinkLogicalPartitionCreateInput{
		Name:        name,
		Description: description,
		TenantOrg:   org,
		SiteID:      site.ID,
		TenantID:    tenant.ID,
		Status:      status,
		CreatedBy:   user.ID,
	})

	assert.Nil(t, err)

	return nvllp
}

func TestCreateVPCHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type expectedStatusDetail struct {
		status  string
		message string
	}
	type args struct {
		reqData                    *model.APIVpcCreateRequest
		reqOrg                     string
		reqUser                    *cdbm.User
		respCode                   int
		respMessage                string
		expectedStatus             string
		expectedVni                *int
		expectedVirtualizationType string
		expectedRoutingProfile     *string
		expectedStatusDetails      []expectedStatusDetail
	}

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	testVPCSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org"
	tnOrgRoles := []string{authz.TenantAdminRole}

	ipu := testVPCBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	tnu := testVPCBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn := testVPCBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu)
	// Routing-profile write privilege is resolved site-scoped. TenantSite
	// associations without an explicit override inherit this Ready TenantAccount
	// default.
	_ = common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip, &tn.ID, tnOrg, cdbm.TenantAccountStatusReady, tnu)

	tnu2 := testVPCBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg, tnOrgRoles)
	tn2 := testVPCBuildTenant(t, dbSession, "test-tenant-2", tnOrg, tnu2)

	tnOrg3 := "test-tenant-org-3"
	tnu3 := testVPCBuildUser(t, dbSession, "test-starfleet-id-4", tnOrg3, tnOrgRoles)
	tn3 := testVPCBuildTenant(t, dbSession, "test-tenant-3", tnOrg3, tnu3)

	st1 := testVPCBuildSite(t, dbSession, ip, "test-site-1", true, true, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st1)
	st1, err := cdbm.NewSiteDAO(dbSession).Update(ctx, nil, cdbm.SiteUpdateInput{
		SiteID: st1.ID,
		Config: &cdbm.SiteConfigUpdateInput{VpcSlaac: cutil.GetPtr(true)},
	})
	require.NoError(t, err)

	st2 := testVPCBuildSite(t, dbSession, ip, "test-site-2", true, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st2)

	st3 := testVPCBuildSite(t, dbSession, ip, "test-site-3", false, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st3)

	st4 := testVPCBuildSite(t, dbSession, ip, "test-site-4", true, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st4)

	st5 := testVPCBuildSite(t, dbSession, ip, "test-site-5", true, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st5)
	_, err = dbSession.DB.NewUpdate().Model(st5).Set("config = config - 'vpc_slaac'").WherePK().Exec(ctx)
	require.NoError(t, err)

	al := testVPCSiteBuildAllocation(t, dbSession, st1, tn, "test-allocation", ipu)
	assert.NotNil(t, al)

	al2 := testVPCSiteBuildAllocation(t, dbSession, st3, tn, "test-allocation-3", ipu)
	assert.NotNil(t, al2)

	al3 := testVPCSiteBuildAllocation(t, dbSession, st1, tn3, "test-allocation-tenant-3", ipu)
	assert.NotNil(t, al3)

	al4 := testVPCSiteBuildAllocation(t, dbSession, st4, tn, "test-allocation-4", ipu)
	assert.NotNil(t, al4)

	al5 := testVPCSiteBuildAllocation(t, dbSession, st5, tn, "test-allocation-5", ipu)
	assert.NotNil(t, al5)

	// Associate tenant 1 with site 1; the unset override inherits the account default.
	ts1t1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st1.ID, tnu.ID)
	assert.NotNil(t, ts1t1)

	// Associate tenant 1 with site 2; the unset override inherits the account default.
	ts2t1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st2.ID, tnu.ID)
	assert.NotNil(t, ts2t1)

	// Associate tenant 2 with site 1
	ts1t2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn2.ID, st1.ID, tnu2.ID)
	assert.NotNil(t, ts1t2)

	// Associate tenant 2 with site 2
	ts2t2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn2.ID, st2.ID, tnu2.ID)
	assert.NotNil(t, ts2t2)

	// Associate tenant 3 with site 1
	ts1t3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg3, tn3.ID, st1.ID, tnu3.ID)
	assert.NotNil(t, ts1t3)

	ts4t1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st4.ID, tnu.ID)
	assert.NotNil(t, ts4t1)

	ts5t1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st5.ID, tnu.ID)
	assert.NotNil(t, ts5t1)

	// NSG for tenant 1 on site 1
	nsgTenant1Site1 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-1", tn, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant1Site1)

	// NSG for tenant 1 on site 2
	nsgTenant1Site2 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-2", tn, st2, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant1Site2)

	// NSG for tenant 2 on site 1
	nsgTenant2Site1 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-3", tn2, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant2Site1)

	// NVLink Logical Partition for tenant 1 on site 1
	nvllp1 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-1", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg, st1, tn, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp1)

	existingVPCSt1 := testVPCBuildVPC(t, dbSession, "test-vpc", ip, tn, st1, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(nvllp1.ID), map[string]string{"zone": "west1"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, existingVPCSt1)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Mock per-Site client for st3
	tst3 := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st1.ID.String()] = tsc
	scp.IDClientMap[st2.ID.String()] = tsc
	scp.IDClientMap[st3.ID.String()] = tst3

	vpcWithAllocatedVniName := "Test VPC with allocated VNI"
	vpcWithRoutingProfileName := "Test VPC routing profile"
	vpcWithRoutingProfileOverridesName := "Test VPC routing profile overrides"
	vpcWithResolvedRoutingProfileName := "Test VPC resolved routing profile"
	vpcWithUnpersistedResolvedRoutingProfileName := "Test VPC unpersisted resolved routing profile"
	vpcWithUnpersistedResolvedRoutingProfileID := uuid.New()
	_, err = dbSession.DB.Exec(`
		ALTER TABLE vpc
		ADD CONSTRAINT vpc_test_reject_resolved_routing_profile_persistence
		CHECK (name <> 'Test VPC unpersisted resolved routing profile' OR routing_profile IS NULL)
	`)
	require.NoError(t, err)
	allocatedVni := uint32(7301)
	expectedAllocatedVni := int(allocatedVni)

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	wrunWithAllocatedVni := &tmocks.WorkflowRun{}
	wrunWithAllocatedVni.On("GetID").Return(wid)
	wrunWithAllocatedVni.Mock.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		controllerVpc, ok := args.Get(1).(*corev1.Vpc)
		if ok {
			controllerVpc.Status = &corev1.VpcStatus{
				Vni: &allocatedVni,
			}
		}
	}).Return(nil)

	wrunWithEffectiveProfile := &tmocks.WorkflowRun{}
	wrunWithEffectiveProfile.On("GetID").Return(wid)
	wrunWithEffectiveProfile.Mock.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		controllerVpc, ok := args.Get(1).(*corev1.Vpc)
		if ok {
			controllerVpc.Status = &corev1.VpcStatus{
				EffectiveRoutingProfile: &corev1.VpcEffectiveRoutingProfile{
					LeakDefaultRouteFromUnderlay: true,
					Internal:                     true,
					AccessTier:                   6,
				},
			}
		}
	}).Return(nil)

	wrunWithResolvedRoutingProfile := &tmocks.WorkflowRun{}
	wrunWithResolvedRoutingProfile.On("GetID").Return(wid)
	wrunWithResolvedRoutingProfile.Mock.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		controllerVpc, ok := args.Get(1).(*corev1.Vpc)
		if ok {
			controllerVpc.Config = &corev1.VpcConfig{
				RoutingProfileType: cutil.GetPtr("EXTERNAL"),
			}
		}
	}).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	unavailableVpcID := uuid.New()
	unavailableVpcName := "Test VPC unavailable after dispatch"
	unavailableErr := errors.New("Core unavailable after VPC create dispatch")
	unavailableRun := &tmocks.WorkflowRun{}
	unavailableRun.On("GetID").Return("test-vpc-unavailable-workflow-id")
	unavailableRun.On("Get", mock.Anything, mock.Anything).Return(
		tp.NewNonRetryableApplicationError(unavailableErr.Error(), swe.ErrTypeNICoUnavailable, unavailableErr),
	)
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateVPCV2", mock.MatchedBy(func(req *corev1.VpcCreationRequest) bool {
			return req != nil && req.Name == unavailableVpcName
		})).Return(unavailableRun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateVPCV2", mock.MatchedBy(func(req *corev1.VpcCreationRequest) bool {
			return req != nil && req.Name == vpcWithAllocatedVniName
		})).Return(wrunWithAllocatedVni, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateVPCV2", mock.MatchedBy(func(req *corev1.VpcCreationRequest) bool {
			return req != nil && (req.Name == vpcWithRoutingProfileName || req.Name == vpcWithRoutingProfileOverridesName)
		})).Return(wrunWithEffectiveProfile, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateVPCV2", mock.MatchedBy(func(req *corev1.VpcCreationRequest) bool {
			return req != nil && (req.Name == vpcWithResolvedRoutingProfileName || req.Name == vpcWithUnpersistedResolvedRoutingProfileName)
		})).Return(wrunWithResolvedRoutingProfile, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateVPCV2", mock.MatchedBy(func(req *corev1.VpcCreationRequest) bool {
			return req == nil || (req.Name != unavailableVpcName && req.Name != vpcWithAllocatedVniName && req.Name != vpcWithRoutingProfileName && req.Name != vpcWithRoutingProfileOverridesName && req.Name != vpcWithResolvedRoutingProfileName && req.Name != vpcWithUnpersistedResolvedRoutingProfileName)
		})).Return(wrun, nil)

	// Mock timeout error
	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")

	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tst3.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateVPCV2", mock.Anything).Return(wruntimeout, nil)

	tst3.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)
	routingProfileOverrides := &model.APIVpcRoutingProfileOverrides{
		RouteTargetImports:           &model.APIVpcRouteTargets{{ASN: 64512, VNI: 559}},
		LeakDefaultRouteFromUnderlay: cutil.GetPtr(false),
		AllowedAnycastPrefixes:       &[]string{"192.0.2.1/24"},
	}

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
		expectNoMutation   bool
		expectRolledBack   bool
	}{
		{
			name: "test VPC create API endpoint rejects power resource group when DPS power management is disabled",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:               "Test VPC rejected power resource group",
					SiteID:             st1.ID.String(),
					PowerResourceGroup: cutil.GetPtr("resource-group"),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusPreconditionFailed,
				respMessage: "Site does not have DPS power management enabled",
			},
			wantErr:          false,
			expectNoMutation: true,
		},
		{
			name: "test VPC create API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test VPC",
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					SlaacEnabled:              cutil.GetPtr(true),
					NetworkSecurityGroupID:    &nsgTenant1Site1.ID,
					Vni:                       cutil.GetPtr(555),
					Labels: map[string]string{
						"vpc-dpu-zone": "east1",
						"vpc-gpu-zone": "west1",
					},
					NVLinkLogicalPartitionID: cutil.GetPtr(nvllp1.ID.String()),
				},
				reqOrg:         tnOrg,
				reqUser:        tnu,
				respCode:       http.StatusCreated,
				expectedStatus: cdbm.VpcStatusProvisioning,
				expectedStatusDetails: []expectedStatusDetail{
					{
						status:  cdbm.VpcStatusProvisioning,
						message: "VPC provisioning has been initiated on Site",
					},
				},
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint defaults SLAAC VPC to FNN on native networking site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:         "Test SLAAC VPC default FNN",
					SiteID:       st1.ID.String(),
					SlaacEnabled: cutil.GetPtr(true),
				},
				reqOrg:                     tnOrg,
				reqUser:                    tnu,
				respCode:                   http.StatusCreated,
				expectedStatus:             cdbm.VpcStatusProvisioning,
				expectedVirtualizationType: cdbm.VpcFNN,
				expectedStatusDetails: []expectedStatusDetail{
					{
						status:  cdbm.VpcStatusProvisioning,
						message: "VPC provisioning has been initiated on Site",
					},
				},
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint returns Core-resolved routing profile",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      vpcWithResolvedRoutingProfileName,
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
				},
				reqOrg:                 tnOrg,
				reqUser:                tnu,
				respCode:               http.StatusCreated,
				expectedStatus:         cdbm.VpcStatusProvisioning,
				expectedRoutingProfile: cutil.GetPtr(model.APIVpcRoutingProfileExternal),
				expectedStatusDetails: []expectedStatusDetail{
					{
						status:  cdbm.VpcStatusProvisioning,
						message: "VPC provisioning has been initiated on Site",
					},
				},
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint rolls back when Core-resolved routing profile cannot be persisted",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					ID:                        &vpcWithUnpersistedResolvedRoutingProfileID,
					Name:                      vpcWithUnpersistedResolvedRoutingProfileName,
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					SlaacEnabled:              cutil.GetPtr(true),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusInternalServerError,
				respMessage: "Failed to persist Core-resolved VPC routing profile",
			},
			wantErr:            false,
			verifyChildSpanner: true,
			expectRolledBack:   true,
		},
		{
			name: "test VPC create API endpoint rejects SLAAC when Site config inventory stores false",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test SLAAC VPC unsupported by Site",
					SiteID:                    st4.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					SlaacEnabled:              cutil.GetPtr(true),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusPreconditionFailed,
				respMessage: "Site does not advertise support for SLAAC-enabled VPCs",
			},
			wantErr:          false,
			expectNoMutation: true,
		},
		{
			name: "test VPC create API endpoint rejects SLAAC when Site config inventory omits the capability",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test SLAAC VPC without cached Site capability",
					SiteID:                    st5.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					SlaacEnabled:              cutil.GetPtr(true),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusPreconditionFailed,
				respMessage: "Site does not advertise support for SLAAC-enabled VPCs",
			},
			wantErr:          false,
			expectNoMutation: true,
		},
		{
			name: "test VPC create API endpoint rolls back when Core is unavailable after dispatch",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					ID:                        &unavailableVpcID,
					Name:                      unavailableVpcName,
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					SlaacEnabled:              cutil.GetPtr(true),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusServiceUnavailable,
				respMessage: unavailableErr.Error(),
			},
			wantErr:          false,
			expectRolledBack: true,
		},
		{
			name: "test VPC create API endpoint rejects SLAAC for explicit ethernet virtualization",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test SLAAC VPC explicit ethernet",
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcEthernetVirtualizer),
					SlaacEnabled:              cutil.GetPtr(true),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "`slaacEnabled` is only supported when network virtualization type is `FNN`",
			},
			wantErr:          false,
			expectNoMutation: true,
		},
		{
			name: "test VPC create API endpoint rejects SLAAC when virtualization defaults to ethernet",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:         "Test SLAAC VPC default ethernet",
					SiteID:       st3.ID.String(),
					SlaacEnabled: cutil.GetPtr(true),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "`slaacEnabled` is only supported when network virtualization type is `FNN`",
			},
			wantErr:          false,
			expectNoMutation: true,
		},
		// Override-only writes use the same site-scoped privilege as named profiles.
		{
			name: "test VPC create API endpoint with routing profile overrides success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      vpcWithRoutingProfileOverridesName,
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					RoutingProfileOverrides:   routingProfileOverrides,
				},
				reqOrg:         tnOrg,
				reqUser:        tnu,
				respCode:       http.StatusCreated,
				expectedStatus: cdbm.VpcStatusProvisioning,
				expectedStatusDetails: []expectedStatusDetail{
					{
						status:  cdbm.VpcStatusProvisioning,
						message: "VPC provisioning has been initiated on Site",
					},
				},
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint returns allocated VNI from Site workflow response",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      vpcWithAllocatedVniName,
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					NetworkSecurityGroupID:    &nsgTenant1Site1.ID,
					Labels: map[string]string{
						"vpc-dpu-zone": "east1",
						"vpc-gpu-zone": "west1",
					},
					NVLinkLogicalPartitionID: cutil.GetPtr(nvllp1.ID.String()),
				},
				reqOrg:         tnOrg,
				reqUser:        tnu,
				respCode:       http.StatusCreated,
				expectedStatus: cdbm.VpcStatusReady,
				expectedVni:    &expectedAllocatedVni,
				expectedStatusDetails: []expectedStatusDetail{
					{
						status:  cdbm.VpcStatusProvisioning,
						message: "VPC provisioning has been initiated on Site",
					},
					{
						status:  cdbm.VpcStatusReady,
						message: "VPC is ready for use",
					},
				},
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		// Override-only writes must not bypass the named-profile privilege gate.
		{
			name: "test VPC create API endpoint rejects routing profile overrides without targeted instance creation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test VPC restricted routing profile overrides",
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					RoutingProfileOverrides:   routingProfileOverrides,
				},
				reqOrg:      tnOrg3,
				reqUser:     tnu3,
				respCode:    http.StatusForbidden,
				respMessage: "Tenant does not have sufficient privileges to set `routingProfileOverrides`",
			},
			wantErr:          false,
			expectNoMutation: true,
		},
		// A privileged FNN create forwards and persists every supplied override.
		{
			name: "test VPC create API endpoint with routing profile success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      vpcWithRoutingProfileName,
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					NetworkSecurityGroupID:    &nsgTenant1Site1.ID,
					Vni:                       cutil.GetPtr(559),
					RoutingProfile:            cutil.GetPtr(model.APIVpcRoutingProfileInternal),
					RoutingProfileOverrides:   routingProfileOverrides,
					Labels: map[string]string{
						"vpc-dpu-zone": "east1",
						"vpc-gpu-zone": "west1",
					},
					NVLinkLogicalPartitionID: cutil.GetPtr(nvllp1.ID.String()),
				},
				reqOrg:         tnOrg,
				reqUser:        tnu,
				respCode:       http.StatusCreated,
				expectedStatus: cdbm.VpcStatusProvisioning,
				expectedStatusDetails: []expectedStatusDetail{
					{
						status:  cdbm.VpcStatusProvisioning,
						message: "VPC provisioning has been initiated on Site",
					},
				},
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint accepts site-configured routing profile",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test VPC site-configured routing profile",
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					RoutingProfile:            cutil.GetPtr("tenant-edge"),
				},
				reqOrg:         tnOrg,
				reqUser:        tnu,
				respCode:       http.StatusCreated,
				expectedStatus: cdbm.VpcStatusProvisioning,
				expectedStatusDetails: []expectedStatusDetail{
					{
						status:  cdbm.VpcStatusProvisioning,
						message: "VPC provisioning has been initiated on Site",
					},
				},
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint rejects routing profile for tenant without targeted instance creation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test VPC restricted routing profile",
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					RoutingProfile:            cutil.GetPtr(model.APIVpcRoutingProfileInternal),
				},
				reqOrg:      tnOrg3,
				reqUser:     tnu3,
				respCode:    http.StatusForbidden,
				respMessage: "Tenant does not have sufficient privileges to set `routingProfile`",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint rejects routing profile for ethernet virtualization",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test VPC ethernet routing profile",
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcEthernetVirtualizer),
					RoutingProfile:            cutil.GetPtr(model.APIVpcRoutingProfileInternal),
					NetworkSecurityGroupID:    &nsgTenant1Site1.ID,
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "`routingProfile` is only supported when `networkVirtualizationType` is FNN",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint Flat virtualization success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test Flat VPC",
					Description:               cutil.GetPtr("Flat VPC for zero-DPU instances"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFlat),
					SlaacEnabled:              cutil.GetPtr(false),
				},
				reqOrg:         tnOrg,
				reqUser:        tnu,
				respCode:       http.StatusCreated,
				expectedStatus: cdbm.VpcStatusProvisioning,
				expectedStatusDetails: []expectedStatusDetail{
					{
						status:  cdbm.VpcStatusProvisioning,
						message: "VPC provisioning has been initiated on Site",
					},
				},
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint rejects routing profile for Flat virtualization",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test Flat VPC with routing profile",
					Description:               cutil.GetPtr("Flat VPC with disallowed routing profile"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFlat),
					RoutingProfile:            cutil.GetPtr(model.APIVpcRoutingProfileInternal),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "`routingProfile` is only supported when `networkVirtualizationType` is FNN",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint original success payload fails with routing profile on ethernet virtualization",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test VPC",
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcEthernetVirtualizer),
					NetworkSecurityGroupID:    &nsgTenant1Site1.ID,
					Vni:                       cutil.GetPtr(555),
					RoutingProfile:            cutil.GetPtr(model.APIVpcRoutingProfileInternal),
					Labels: map[string]string{
						"vpc-dpu-zone": "east1",
						"vpc-gpu-zone": "west1",
					},
					NVLinkLogicalPartitionID: cutil.GetPtr(nvllp1.ID.String()),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "`routingProfile` is only supported when `networkVirtualizationType` is FNN",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint rejects routing profile when resolved network virtualization type defaults to ethernet",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:           "Test VPC default ethernet routing profile",
					Description:    cutil.GetPtr("Test VPC Description"),
					SiteID:         st3.ID.String(),
					RoutingProfile: cutil.GetPtr(model.APIVpcRoutingProfileInternal),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "`routingProfile` can only be specified if network virtualization type is set to `FNN`, or Site has native networking enabled and no network virtualization type is specified",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		// Handler-side defaulting reports the resolved unsupported type.
		{
			name: "test VPC create API endpoint rejects routing profile overrides when type defaults to ethernet",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                    "Test VPC default ethernet routing profile overrides",
					SiteID:                  st3.ID.String(),
					RoutingProfileOverrides: routingProfileOverrides,
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "Routing profile overrides are not supported for network virtualization type: ETHERNET_VIRTUALIZER",
			},
			wantErr:          false,
			expectNoMutation: true,
		},
		{
			name: "test VPC create API endpoint with explicit VPC ID success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					ID:                        cutil.GetPtr(uuid.New()),
					Name:                      "Test VPC 2",
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					NetworkSecurityGroupID:    &nsgTenant1Site1.ID,
					Vni:                       cutil.GetPtr(557),
					Labels: map[string]string{
						"vpc-dpu-zone": "east1",
						"vpc-gpu-zone": "west1",
					},
					NVLinkLogicalPartitionID: cutil.GetPtr(nvllp1.ID.String()),
				},
				reqOrg:         tnOrg,
				reqUser:        tnu,
				respCode:       http.StatusCreated,
				expectedStatus: cdbm.VpcStatusProvisioning,
				expectedStatusDetails: []expectedStatusDetail{
					{
						status:  cdbm.VpcStatusProvisioning,
						message: "VPC provisioning has been initiated on Site",
					},
				},
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint with explicit VPC ID fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					ID:                        &existingVPCSt1.ID,
					Name:                      "Test VPC 3",
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					NetworkSecurityGroupID:    &nsgTenant1Site1.ID,
					Vni:                       cutil.GetPtr(556),
					Labels: map[string]string{
						"vpc-dpu-zone": "east1",
						"vpc-gpu-zone": "west1",
					},
					NVLinkLogicalPartitionID: cutil.GetPtr(nvllp1.ID.String()),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusConflict,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint NSG not owned by tenant - fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test VPC bad nsg tenant",
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					NetworkSecurityGroupID:    &nsgTenant2Site1.ID,

					Labels: map[string]string{
						"vpc-dpu-zone": "east1",
						"vpc-gpu-zone": "west1",
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusForbidden,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint NSG not owned by site - fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test VPC bad nsg tenant",
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					NetworkSecurityGroupID:    &nsgTenant1Site2.ID,

					Labels: map[string]string{
						"vpc-dpu-zone": "east1",
						"vpc-gpu-zone": "west1",
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusForbidden,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint NSG not found - fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test VPC bad nsg tenant",
					Description:               cutil.GetPtr("Test VPC Description"),
					SiteID:                    st1.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
					NetworkSecurityGroupID:    cutil.GetPtr(uuid.NewString()),

					Labels: map[string]string{
						"vpc-dpu-zone": "east1",
						"vpc-gpu-zone": "west1",
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusBadRequest,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "error when VPC with same name already exists",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:        "Test VPC",
					Description: cutil.GetPtr("Test VPC Description"),
					SiteID:      st1.ID.String(),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusConflict,
			},
			wantErr: false,
		},
		{
			name: "test VPC create API endpoint failure, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:        "Test VPC",
					Description: cutil.GetPtr("Test VPC Description"),
					SiteID:      st1.ID.String(),
				},
				reqOrg:   ipOrg,
				reqUser:  ipu,
				respCode: http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test VPC create API endpoint failure, invalid Site ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:        "Test VPC",
					Description: cutil.GetPtr("Test VPC Description"),
					SiteID:      uuid.NewString(),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test VPC create API endpoint failure, Tenant has no Site Allocation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:        "Test VPC",
					Description: cutil.GetPtr("Test VPC Description"),
					SiteID:      st2.ID.String(),
				},
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test VPC create API endpoint fail, site hasn't been enabled for FNN",
			fields: fields{
				dbSession: dbSession,
				tc:        tst3,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:                      "Test VPC 3",
					Description:               cutil.GetPtr("Test VPC Description 3"),
					SiteID:                    st3.ID.String(),
					NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "Site specified in request data must have native networking enabled in order to create FNN VPCs",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint fail, workflow timeout",
			fields: fields{
				dbSession: dbSession,
				tc:        tst3,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:        "Test VPC 3",
					Description: cutil.GetPtr("Test VPC Description 3"),
					SiteID:      st3.ID.String(),
					Labels: map[string]string{
						"vpc-dpu-zone": "east1",
						"vpc-gpu-zone": "west1",
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusInternalServerError,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC create API endpoint failure, invalid label key",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcCreateRequest{
					Name:        "Test VPC",
					Description: cutil.GetPtr("Test VPC Description"),
					SiteID:      st1.ID.String(),
					Labels: map[string]string{
						"ygsV9MoUjep1rCwbQskkF9wfMolE3oDTCcxuYSJCx9TLKepCIku9pnHfIkxCxHkb7ucbsBL4hyLqQaHoEqpTBmfoX4Un7sGvQdHGZ7nb68JJEJ3ocFAtyCMCBt66z3ldnTqp8SXXOIhNsOh35MLYQjI8557Pu6o91TsEBqyTz0yz68HHmfNgJoreHpXfeujq4cpElUXXbQ3xfFICkNyghXgFZ0MLs2o0u1Nd29aB113X5g3FKJBCskW6eBULNmeFFG61DMM37q": "east1",
					},
				},
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "label key must contain at least 1 character and a maximum of 255 characters",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tsc.Calls = nil
			tst3.Calls = nil

			csh := CreateVPCHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       scp,
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

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("CreateVPCHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("CreateVPCHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respMessage != "" {
				assert.Contains(t, rec.Body.String(), tt.args.respMessage)
			}

			tsc.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "InvokeCoreGRPC", mock.Anything)
			tst3.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "InvokeCoreGRPC", mock.Anything)
			if tt.expectNoMutation {
				// Authorization and compatibility failures must precede persistence and workflow dispatch.
				persistedVpcs, total, gerr := cdbm.NewVpcDAO(tt.fields.dbSession).GetAll(ctx, nil, cdbm.VpcFilterInput{Name: &tt.args.reqData.Name}, paginator.PageInput{}, nil)
				require.NoError(t, gerr)
				assert.Zero(t, total)
				assert.Empty(t, persistedVpcs)
				requestMatcher := mock.MatchedBy(func(req *corev1.VpcCreationRequest) bool {
					return req != nil && req.Name == tt.args.reqData.Name
				})
				tsc.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "CreateVPCV2", requestMatcher)
				tst3.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "CreateVPCV2", requestMatcher)
			}
			if tt.expectRolledBack {
				persistedVpcs, total, gerr := cdbm.NewVpcDAO(tt.fields.dbSession).GetAll(ctx, nil, cdbm.VpcFilterInput{Name: &tt.args.reqData.Name}, paginator.PageInput{}, nil)
				require.NoError(t, gerr)
				assert.Zero(t, total)
				assert.Empty(t, persistedVpcs)

				require.NotNil(t, tt.args.reqData.ID)
				statusDetails, total, gerr := cdbm.NewStatusDetailDAO(tt.fields.dbSession).GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{tt.args.reqData.ID.String()}}, paginator.PageInput{})
				require.NoError(t, gerr)
				assert.Zero(t, total)
				assert.Empty(t, statusDetails)

				forwardedRequestMatcher := mock.MatchedBy(func(req *corev1.VpcCreationRequest) bool {
					return req != nil && req.Name == tt.args.reqData.Name && req.SlaacEnabled != nil && *req.SlaacEnabled
				})
				tsc.AssertCalled(t, "ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "CreateVPCV2", forwardedRequestMatcher)
			}
			if tt.args.respCode != http.StatusCreated {
				return
			}

			rst := &model.APIVpc{}

			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			require.NoError(t, serr)
			responseFields := struct {
				SlaacEnabled *bool `json:"slaacEnabled"`
			}{}
			serr = json.Unmarshal(rec.Body.Bytes(), &responseFields)
			require.NoError(t, serr)

			assert.Equal(t, rst.Name, tt.args.reqData.Name)
			assert.True(t, tt.args.reqData.ID == nil || rst.ID == tt.args.reqData.ID.String(), "%+v != %+v", rst.ID, tt.args.reqData.ID)
			if tt.args.reqData.Vni != nil {
				assert.NotNil(t, rst.RequestedVni)
				assert.Equal(t, *tt.args.reqData.Vni, *rst.RequestedVni)
			}
			if tt.args.reqData.Description != nil {
				assert.NotNil(t, rst.Description)
				assert.Equal(t, *rst.Description, *tt.args.reqData.Description)
			} else {
				assert.Nil(t, rst.Description)
			}
			expectedRoutingProfile := tt.args.reqData.RoutingProfile
			if tt.args.expectedRoutingProfile != nil {
				expectedRoutingProfile = tt.args.expectedRoutingProfile
			}
			assert.Equal(t, expectedRoutingProfile, rst.RoutingProfile)
			assert.Equal(t, tt.args.reqData.RoutingProfileOverrides, rst.RoutingProfileOverrides)
			expectedSlaacEnabled := tt.args.reqData.SlaacEnabled != nil && *tt.args.reqData.SlaacEnabled
			assert.Equal(t, expectedSlaacEnabled, rst.SlaacEnabled)
			require.NotNil(t, responseFields.SlaacEnabled)
			assert.Equal(t, expectedSlaacEnabled, *responseFields.SlaacEnabled)
			expectedVirtualizationType := cdbm.VpcEthernetVirtualizer
			if tt.args.reqData.NetworkVirtualizationType != nil {
				expectedVirtualizationType = *tt.args.reqData.NetworkVirtualizationType
			}
			if tt.args.expectedVirtualizationType != "" {
				expectedVirtualizationType = tt.args.expectedVirtualizationType
			}
			require.NotNil(t, rst.NetworkVirtualizationType)
			assert.Equal(t, expectedVirtualizationType, *rst.NetworkVirtualizationType)
			assert.Equal(t, tt.args.expectedStatus, rst.Status)
			require.Len(t, rst.StatusHistory, len(tt.args.expectedStatusDetails))
			for i, expectedStatusDetail := range tt.args.expectedStatusDetails {
				assert.Equal(t, expectedStatusDetail.status, rst.StatusHistory[i].Status)
				require.NotNil(t, rst.StatusHistory[i].Message)
				assert.Equal(t, expectedStatusDetail.message, *rst.StatusHistory[i].Message)
			}
			if tt.args.expectedVni != nil {
				require.NotNil(t, rst.Vni)
				assert.Equal(t, *tt.args.expectedVni, *rst.Vni)
			} else {
				assert.Nil(t, rst.Vni)
			}

			if tt.args.reqData.NVLinkLogicalPartitionID != nil {
				assert.Equal(t, *rst.NVLinkLogicalPartitionID, *tt.args.reqData.NVLinkLogicalPartitionID)
			} else {
				assert.Nil(t, rst.NVLinkLogicalPartitionID)
			}

			if tt.args.reqData.Labels != nil {
				assert.Equal(t, len(rst.Labels), len(tt.args.reqData.Labels))
			}

			// Read the row independently so the create response cannot hide a persistence defect.
			persistedVpc, gerr := cdbm.NewVpcDAO(tt.fields.dbSession).GetByID(ctx, nil, uuid.MustParse(rst.ID), nil)
			require.NoError(t, gerr)
			assert.Equal(t, expectedSlaacEnabled, persistedVpc.SlaacEnabled)
			require.NotNil(t, persistedVpc.NetworkVirtualizationType)
			assert.Equal(t, expectedVirtualizationType, *persistedVpc.NetworkVirtualizationType)
			if expectedRoutingProfile != nil {
				require.NotNil(t, persistedVpc.RoutingProfile)
				assert.Equal(t, model.NormalizeAPIVpcRoutingProfileForSite(*expectedRoutingProfile), *persistedVpc.RoutingProfile)
			} else {
				assert.Nil(t, persistedVpc.RoutingProfile)
			}
			assert.Equal(t, tt.args.reqData.RoutingProfileOverrides.ToDB(), persistedVpc.RoutingProfileOverrides)
			if tt.args.reqData.RoutingProfileOverrides != nil {
				// Effective state returned without a VNI is cached and exposed to this privileged tenant.
				require.NotNil(t, rst.EffectiveRoutingProfile)
				assert.Equal(t, uint32(6), rst.EffectiveRoutingProfile.AccessTier)
				require.NotNil(t, persistedVpc.EffectiveRoutingProfile)
				assert.Equal(t, uint32(6), persistedVpc.EffectiveRoutingProfile.AccessTier)
			}

			assert.True(t, tsc.AssertCalled(t, "ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "CreateVPCV2", mock.MatchedBy(func(req *corev1.VpcCreationRequest) bool {
				if req == nil {
					return false
				}
				if !proto.Equal(req.RoutingProfileOverrides, tt.args.reqData.RoutingProfileOverrides.ToDB().ToProto()) {
					return false
				}
				if tt.args.reqData.SlaacEnabled == nil {
					if req.SlaacEnabled != nil {
						return false
					}
				} else if req.SlaacEnabled == nil || *req.SlaacEnabled != *tt.args.reqData.SlaacEnabled {
					return false
				}
				if req.NetworkVirtualizationType == nil {
					return false
				}
				expectedProtoVirtualizationType := corev1.VpcVirtualizationType_ETHERNET_VIRTUALIZER
				switch expectedVirtualizationType {
				case cdbm.VpcFNN:
					expectedProtoVirtualizationType = corev1.VpcVirtualizationType_FNN
				case cdbm.VpcFlat:
					expectedProtoVirtualizationType = corev1.VpcVirtualizationType_FLAT
				}
				if *req.NetworkVirtualizationType != expectedProtoVirtualizationType {
					return false
				}
				if tt.args.reqData.RoutingProfile == nil {
					return req.RoutingProfileType == nil
				}
				if req.RoutingProfileType == nil {
					return false
				}
				return *req.RoutingProfileType == model.NormalizeAPIVpcRoutingProfileForSite(*tt.args.reqData.RoutingProfile)
			})))

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestUpdateVPCHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		reqData     *model.APIVpcUpdateRequest
		reqOrg      string
		reqUser     *cdbm.User
		reqVPCID    string
		reqVPC      *cdbm.Vpc
		respCode    int
		respMessage string
	}

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	testVPCSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org"
	tnOrgRoles := []string{authz.TenantAdminRole}

	ipu := testVPCBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	tnu := testVPCBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn := testVPCBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu)
	_ = common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip, &tn.ID, tnOrg, cdbm.TenantAccountStatusReady, tnu)

	tnu2 := testVPCBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg, tnOrgRoles)
	tn2 := testVPCBuildTenant(t, dbSession, "test-tenant-2", tnOrg, tnu2)

	tnOrg3 := "test-tenant-org-3"
	tnu3 := testVPCBuildUser(t, dbSession, "test-starfleet-id-4", tnOrg3, tnOrgRoles)
	tn3 := testVPCBuildTenant(t, dbSession, "test-tenant-3", tnOrg3, tnu3)

	st := testVPCBuildSite(t, dbSession, ip, "test-site-1", false, true, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st)

	// Site with no allocations
	st2 := testVPCBuildSite(t, dbSession, ip, "test-site-2", false, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st2)

	// Site with no allocations
	st3 := testVPCBuildSite(t, dbSession, ip, "test-site-3", false, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st3)

	al := testVPCSiteBuildAllocation(t, dbSession, st, tn, "test-allocation", ipu)
	assert.NotNil(t, al)

	al1 := testVPCSiteBuildAllocation(t, dbSession, st2, tn, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	// NVLink Logical Partition for tenant 1 on site 1
	nvllp1 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-1", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg, st, tn, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp1)

	nvllp2 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-2", cutil.GetPtr("Test NVLink Logical Partition 2"), tnOrg, st, tn, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp2)

	vpc := testVPCBuildVPC(t, dbSession, "test-vpc", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(nvllp1.ID), map[string]string{"zone": "west1"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpc)

	vpc2 := testVPCBuildVPC(t, dbSession, "test-vpc-2", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "wes2"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpc2)
	vpcFNN := testVPCBuildVPC(t, dbSession, "test-vpc-fnn", ip, tn, st, cutil.GetPtr(cdbm.VpcFNN), nil, nil, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpcFNN)
	vpcFNN.EffectiveRoutingProfile = &cdbm.VpcEffectiveRoutingProfile{Internal: true, AccessTier: 2}
	testUpdateVPC(t, dbSession, vpcFNN)
	vpcFNNUnprivileged := testVPCBuildVPC(t, dbSession, "test-vpc-fnn-unprivileged", ip, tn3, st, cutil.GetPtr(cdbm.VpcFNN), nil, nil, cdbm.VpcStatusReady, tnu3)
	assert.NotNil(t, vpcFNNUnprivileged)

	vpc3 := testVPCBuildVPC(t, dbSession, "test-vpc-3", ip, tn, st2, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "west3"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpc2)

	vpc4 := testVPCBuildVPC(t, dbSession, "test-vpc-3", ip, tn, st3, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "west6"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpc4)

	// Associate tenant 1 with site 1
	ts1t1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st.ID, tnu.ID)
	assert.NotNil(t, ts1t1)

	// Associate tenant 1 with site 2
	ts2t1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st2.ID, tnu.ID)
	assert.NotNil(t, ts2t1)

	// Associate tenant 2 with site 1
	ts1t2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn2.ID, st.ID, tnu2.ID)
	assert.NotNil(t, ts1t2)

	// Associate the unprivileged tenant with Site 1 without granting targeted instance creation.
	ts1t3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg3, tn3.ID, st.ID, tnu3.ID)
	assert.NotNil(t, ts1t3)

	// Associate tenant 2 with site 2
	ts2t2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn2.ID, st2.ID, tnu2.ID)
	assert.NotNil(t, ts2t2)

	// NSG for tenant 1 on site 1
	nsgTenant1Site1 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-1", tn, st, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant1Site1)

	// NSG for tenant 1 on site 2
	nsgTenant1Site2 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-2", tn, st2, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant1Site2)

	// NSG for tenant 2 on site 1
	nsgTenant2Site1 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-3", tn2, st, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant2Site1)

	vpcWithNSG := testVPCBuildVPC(t, dbSession, "test-vpc-with-nsg", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "west7"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpcWithNSG)
	vpcWithNSG.NetworkSecurityGroupID = cutil.GetPtr(nsgTenant1Site1.ID)
	testUpdateVPC(t, dbSession, vpcWithNSG)

	vpcForNSGSet := testVPCBuildVPC(t, dbSession, "test-vpc-set-nsg", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "west8"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpcForNSGSet)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)
	updateRoutingProfileOverrides := &model.APIVpcRoutingProfileOverrides{
		RouteTargetsOnExports:         &model.APIVpcRouteTargets{{ASN: 64513, VNI: 61}},
		TenantLeakCommunitiesAccepted: cutil.GetPtr(true),
		AcceptedLeaksFromUnderlay:     &[]string{},
	}

	// Mock per-Site client for st3
	tsc := &tmocks.Client{}
	tst := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st.ID.String()] = tsc
	scp.IDClientMap[st2.ID.String()] = tst

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateVPC", mock.Anything).Return(wrun, nil)

	// Mock timeout error
	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")

	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tst.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateVPC", mock.Anything).Return(wruntimeout, nil)

	tst.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name                              string
		fields                            fields
		args                              args
		wantErr                           bool
		verifyChildSpanner                bool
		expectedNVLinkPartitionValue      *string
		expectNVLinkPartitionNil          bool
		expectedNetworkSecurityGroupValue *string
		expectNetworkSecurityGroupNil     bool
		expectedRoutingProfileOverrides   *model.APIVpcRoutingProfileOverrides
		expectNoRoutingProfileMutation    bool
	}{
		{
			name: "test VPC update rejects power resource group when DPS power management is disabled",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					PowerResourceGroup: cutil.GetPtr("resource-group"),
				},
				reqVPCID:    vpc.ID.String(),
				reqVPC:      vpc,
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusPreconditionFailed,
				respMessage: "Site does not have DPS power management enabled",
			},
		},
		// A present object replaces the FNN VPC's full inline definition.
		{
			name: "test VPC update replaces routing profile overrides",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					RoutingProfileOverrides: updateRoutingProfileOverrides,
				},
				reqVPCID: vpcFNN.ID.String(),
				reqVPC:   vpcFNN,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusOK,
			},
			expectedRoutingProfileOverrides: updateRoutingProfileOverrides,
		},
		// Tenants without targeted instance creation cannot replace inline definitions.
		{
			name: "test VPC update rejects routing profile overrides without targeted instance creation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					RoutingProfileOverrides: updateRoutingProfileOverrides,
				},
				reqVPCID:    vpcFNNUnprivileged.ID.String(),
				reqVPC:      vpcFNNUnprivileged,
				reqOrg:      tnOrg3,
				reqUser:     tnu3,
				respCode:    http.StatusForbidden,
				respMessage: "Tenant does not have sufficient privileges to set `routingProfileOverrides`",
			},
			expectNoRoutingProfileMutation: true,
		},
		// Inline definitions are rejected when the persisted VPC is not FNN.
		{
			name: "test VPC update rejects routing profile overrides for ethernet virtualization",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					RoutingProfileOverrides: updateRoutingProfileOverrides,
				},
				reqVPCID:    vpc2.ID.String(),
				reqVPC:      vpc2,
				reqOrg:      tnOrg,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "Routing profile overrides are not supported for network virtualization type: ETHERNET_VIRTUALIZER",
			},
		},
		{
			name: "test VPC update success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:        cutil.GetPtr("test-vpc"),
					Description: cutil.GetPtr("Test VPC Description"),
					Labels: map[string]string{
						"zone": "westnew",
					},
					NVLinkLogicalPartitionID: cutil.GetPtr(nvllp1.ID.String()),
				},
				reqVPCID: vpc.ID.String(),
				reqVPC:   vpc,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusOK,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC update to set NSG - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					NetworkSecurityGroupID: &nsgTenant1Site1.ID,
				},
				reqVPCID: vpcForNSGSet.ID.String(),
				reqVPC:   vpcForNSGSet,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusOK,
			},
			wantErr:                           false,
			verifyChildSpanner:                true,
			expectedNetworkSecurityGroupValue: &nsgTenant1Site1.ID,
		},
		{
			name: "test VPC update NSG with bad tenant - fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:                   cutil.GetPtr(uuid.NewString()),
					Description:            cutil.GetPtr("Test VPC Description"),
					NetworkSecurityGroupID: &nsgTenant2Site1.ID,
					Labels: map[string]string{
						"zone": "westnew",
					},
				},
				reqVPCID: vpc.ID.String(),
				reqVPC:   vpc,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusForbidden,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC update NSG with bad site - fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:                   cutil.GetPtr(uuid.NewString()),
					Description:            cutil.GetPtr("Test VPC Description"),
					NetworkSecurityGroupID: &nsgTenant1Site2.ID,
					Labels: map[string]string{
						"zone": "westnew",
					},
				},
				reqVPCID: vpc.ID.String(),
				reqVPC:   vpc,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusForbidden,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC update NSG with NSG not found - fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:                   cutil.GetPtr(uuid.NewString()),
					Description:            cutil.GetPtr("Test VPC Description"),
					NetworkSecurityGroupID: cutil.GetPtr(uuid.NewString()),
					Labels: map[string]string{
						"zone": "westnew",
					},
				},
				reqVPCID: vpc.ID.String(),
				reqVPC:   vpc,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusBadRequest,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC update to clear NSG - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					NetworkSecurityGroupID: cutil.GetPtr(""),
				},
				reqVPCID: vpcWithNSG.ID.String(),
				reqVPC:   vpcWithNSG,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusOK,
			},
			wantErr:                       false,
			verifyChildSpanner:            true,
			expectNetworkSecurityGroupNil: true,
		},

		{
			name: "test VPC update error due to name clash",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:        cutil.GetPtr("test-vpc-2"),
					Description: cutil.GetPtr("Test VPC Description"),
				},
				reqVPCID: vpc.ID.String(),
				reqVPC:   vpc,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusConflict,
			},
			wantErr: false,
		},
		{
			name: "test VPC update success with same name",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:        cutil.GetPtr("test-vpc-2"),
					Description: cutil.GetPtr("Test VPC Description"),
				},
				reqVPCID: vpc2.ID.String(),
				reqVPC:   vpc2,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusOK,
			},
			wantErr: false,
		},
		{
			name: "test VPC update error, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:        cutil.GetPtr("test-vpc"),
					Description: cutil.GetPtr("Test VPC Description"),
				},
				reqVPCID: vpc.ID.String(),
				reqVPC:   vpc,
				reqOrg:   ipOrg,
				reqUser:  ipu,
				respCode: http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test VPC update error, invalid VPC ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:        cutil.GetPtr("test-vpc"),
					Description: cutil.GetPtr("Test VPC Description"),
				},
				reqVPC:   vpc,
				reqVPCID: "",
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test VPC update error due to no allocations",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:        cutil.GetPtr("test-vpc-3"),
					Description: cutil.GetPtr("Test VPC Description"),
				},
				reqVPCID: vpc4.ID.String(),
				reqVPC:   vpc4,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test VPC update API endpoint fail, workflow timeout",
			fields: fields{
				dbSession: dbSession,
				tc:        tst,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:        cutil.GetPtr("Test VPC 3"),
					Description: cutil.GetPtr("Test VPC Description 3"),
					Labels: map[string]string{
						"vpc-dpu-zone": "east1",
						"vpc-gpu-zone": "west1",
					},
				},
				reqOrg:      tnOrg,
				reqVPCID:    vpc3.ID.String(),
				reqVPC:      vpc3,
				reqUser:     tnu,
				respCode:    http.StatusInternalServerError,
				respMessage: "Failed to perform VPC Update - timeout occurred executing workflow on Site",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC update API endpoint failure, invalid label key",
			fields: fields{
				dbSession: dbSession,
				tc:        tsc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:        cutil.GetPtr("Test VPC"),
					Description: cutil.GetPtr("Test VPC Description"),
					Labels: map[string]string{
						"ygsV9MoUjep1rCwbQskkF9wfMolE3oDTCcxuYSJCx9TLKepCIku9pnHfIkxCxHkb7ucbsBL4hyLqQaHoEqpTBmfoX4Un7sGvQdHGZ7nb68JJEJ3ocFAtyCMCBt66z3ldnTqp8SXXOIhNsOh35MLYQjI8557Pu6o91TsEBqyTz0yz68HHmfNgJoreHpXfeujq4cpElUXXbQ3xfFICkNyghXgFZ0MLs2o0u1Nd29aB113X5g3FKJBCskW6eBULNmeFFG61DMM37q": "east1",
					},
				},
				reqOrg:      tnOrg,
				reqVPCID:    vpc.ID.String(),
				reqVPC:      vpc,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "label key must contain at least 1 character and a maximum of 255 characters",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC update to clear NVLink Logical Partition ID - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:                     cutil.GetPtr("test-vpc"),
					Description:              cutil.GetPtr("Test VPC Description"),
					NVLinkLogicalPartitionID: cutil.GetPtr(""),
				},
				reqVPCID: vpc.ID.String(),
				reqVPC:   vpc,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusOK,
			},
			wantErr:                  false,
			expectNVLinkPartitionNil: true,
		},
		{
			name: "test VPC update to set NVLink Logical Partition ID after clearing - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcUpdateRequest{
					Name:                     cutil.GetPtr("test-vpc"),
					Description:              cutil.GetPtr("Test VPC Description"),
					NVLinkLogicalPartitionID: cutil.GetPtr(nvllp2.ID.String()),
				},
				reqVPCID: vpc.ID.String(),
				reqVPC:   vpc,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusOK,
			},
			wantErr:                      false,
			expectedNVLinkPartitionValue: cutil.GetPtr(nvllp2.ID.String()),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			csh := UpdateVPCHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       scp,
				cfg:       tt.fields.cfg,
			}

			jsonData, _ := json.Marshal(tt.args.reqData)

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(string(jsonData)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/vpc/%v", tt.args.reqOrg, tt.args.reqVPCID))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.reqOrg, tt.args.reqVPCID)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("UpdateVPCHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("UpdateVPCHandler.Handle() resp = %v", rec.Body.String())
			}

			if tt.args.respMessage != "" {
				assert.Contains(t, rec.Body.String(), tt.args.respMessage)
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.expectNoRoutingProfileMutation {
				// Rejected writes must preserve storage and avoid dispatching the update workflow.
				persistedVpc, gerr := cdbm.NewVpcDAO(tt.fields.dbSession).GetByID(ctx, nil, uuid.MustParse(tt.args.reqVPCID), nil)
				require.NoError(t, gerr)
				assert.Nil(t, persistedVpc.RoutingProfileOverrides)
				workflowOptionsMatcher := mock.MatchedBy(func(options temporalClient.StartWorkflowOptions) bool {
					return options.ID == "vpc-update-"+tt.args.reqVPCID
				})
				tsc.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, workflowOptionsMatcher, "UpdateVPC", mock.Anything)
				tst.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, workflowOptionsMatcher, "UpdateVPC", mock.Anything)
			}
			if tt.args.respCode != http.StatusOK {
				return
			}

			rst := &model.APIVpc{}

			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			if serr != nil {
				t.Fatal(serr)
			}

			if tt.args.reqData.Name != nil {
				assert.Equal(t, *tt.args.reqData.Name, rst.Name)
			}
			if tt.args.reqData.Description != nil {
				require.NotNil(t, rst.Description)
				assert.Equal(t, *tt.args.reqData.Description, *rst.Description)
			}
			assert.NotEqual(t, rst.Updated.String(), tt.args.reqVPC.Updated.String())

			if tt.args.reqData.NVLinkLogicalPartitionID != nil {
				if *tt.args.reqData.NVLinkLogicalPartitionID == "" {
					assert.Nil(t, rst.NVLinkLogicalPartitionID)
				} else {
					assert.Equal(t, *rst.NVLinkLogicalPartitionID, *tt.args.reqData.NVLinkLogicalPartitionID)
				}
			}

			if tt.args.reqData.Labels != nil {
				assert.Equal(t, len(rst.Labels), len(tt.args.reqData.Labels))
			}

			if tt.expectedRoutingProfileOverrides != nil {
				// Verify the response and a fresh DB lookup both hold the replacement object.
				assert.Equal(t, tt.expectedRoutingProfileOverrides, rst.RoutingProfileOverrides)
				persistedVpc, gerr := cdbm.NewVpcDAO(tt.fields.dbSession).GetByID(ctx, nil, uuid.MustParse(tt.args.reqVPCID), nil)
				require.NoError(t, gerr)
				assert.Equal(t, tt.expectedRoutingProfileOverrides.ToDB(), persistedVpc.RoutingProfileOverrides)
				assert.Nil(t, persistedVpc.EffectiveRoutingProfile)
			}

			if tt.args.reqData.NetworkSecurityGroupID != nil {
				persistedVpc, err := cdbm.NewVpcDAO(tt.fields.dbSession).GetByID(ctx, nil, uuid.MustParse(tt.args.reqVPCID), nil)
				require.NoError(t, err)
				if *tt.args.reqData.NetworkSecurityGroupID == "" {
					assert.Nil(t, rst.NetworkSecurityGroupID, "Failed to clear VPC NSG ID in response")
					assert.Nil(t, persistedVpc.NetworkSecurityGroupID, "Failed to clear persisted VPC NSG ID")
				} else {
					require.NotNil(t, rst.NetworkSecurityGroupID)
					require.NotNil(t, persistedVpc.NetworkSecurityGroupID)
					assert.Equal(t, *tt.args.reqData.NetworkSecurityGroupID, *rst.NetworkSecurityGroupID)
					assert.Equal(t, *tt.args.reqData.NetworkSecurityGroupID, *persistedVpc.NetworkSecurityGroupID)
				}
			}

			var lastUpdateVPCReq *corev1.VpcUpdateRequest
			if tt.expectedNVLinkPartitionValue != nil || tt.expectNVLinkPartitionNil || tt.expectedNetworkSecurityGroupValue != nil || tt.expectNetworkSecurityGroupNil || tt.expectedRoutingProfileOverrides != nil {
				for i := len(tsc.Mock.Calls) - 1; i >= 0; i-- {
					call := tsc.Mock.Calls[i]
					if call.Method == "ExecuteWorkflow" && len(call.Arguments) >= 4 {
						if wfName, ok := call.Arguments[2].(string); ok && wfName == "UpdateVPC" {
							lastUpdateVPCReq, _ = call.Arguments[3].(*corev1.VpcUpdateRequest)
							break
						}
					}
				}
				require.NotNil(t, lastUpdateVPCReq, "UpdateVPC workflow should have been called")
			}

			if tt.expectNVLinkPartitionNil {
				assert.Nil(t, lastUpdateVPCReq.DefaultNvlinkLogicalPartitionId, "DefaultNvlinkLogicalPartitionId should be nil in workflow request")
			} else if tt.expectedNVLinkPartitionValue != nil {
				require.NotNil(t, lastUpdateVPCReq.DefaultNvlinkLogicalPartitionId, "DefaultNvlinkLogicalPartitionId should be set in workflow request")
				assert.Equal(t, *tt.expectedNVLinkPartitionValue, lastUpdateVPCReq.DefaultNvlinkLogicalPartitionId.Value)
			}
			if tt.expectNetworkSecurityGroupNil {
				assert.Nil(t, lastUpdateVPCReq.NetworkSecurityGroupId, "NetworkSecurityGroupId should be nil in workflow request")
			} else if tt.expectedNetworkSecurityGroupValue != nil {
				require.NotNil(t, lastUpdateVPCReq.NetworkSecurityGroupId, "NetworkSecurityGroupId should be set in workflow request")
				assert.Equal(t, *tt.expectedNetworkSecurityGroupValue, *lastUpdateVPCReq.NetworkSecurityGroupId)
			}
			if tt.expectedRoutingProfileOverrides != nil {
				assert.True(t, proto.Equal(tt.expectedRoutingProfileOverrides.ToDB().ToProto(), lastUpdateVPCReq.RoutingProfileOverrides))
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestUpdateVirtualizationVPCHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		reqData     *model.APIVpcVirtualizationUpdateRequest
		reqOrg      string
		reqUser     *cdbm.User
		reqVPCID    string
		reqVPC      *cdbm.Vpc
		respCode    int
		respMessage string
	}

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	testInstanceSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org"
	tnOrgRoles := []string{authz.TenantAdminRole}

	ipu := testVPCBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	tnu := testVPCBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn := testVPCBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu)

	// Native networking must be enabled on Site for FNN virtualization updates once VPC loads Site relation.
	st := testVPCBuildSite(t, dbSession, ip, "test-site-1", true, true, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st.ID, tnu.ID)
	assert.NotNil(t, ts1)

	// Site with no allocations
	st2 := testVPCBuildSite(t, dbSession, ip, "test-site-2", true, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st2)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st2.ID, tnu.ID)
	assert.NotNil(t, ts2)

	// Site with no allocations
	st3 := testVPCBuildSite(t, dbSession, ip, "test-site-3", false, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st3)

	al := testVPCSiteBuildAllocation(t, dbSession, st, tn, "test-allocation", ipu)
	assert.NotNil(t, al)

	al1 := testVPCSiteBuildAllocation(t, dbSession, st2, tn, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	vpc := testVPCBuildVPC(t, dbSession, "test-vpc", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "west1"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpc)

	vpc2 := testVPCBuildVPC(t, dbSession, "test-vpc-2", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "wes2"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpc2)

	vpc3 := testVPCBuildVPC(t, dbSession, "test-vpc-3", ip, tn, st2, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "west3"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpc2)

	vpc4 := testVPCBuildVPC(t, dbSession, "test-vpc-3", ip, tn, st2, cutil.GetPtr(cdbm.VpcFNN), nil, map[string]string{"zone": "west6"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpc4)

	vpcWithSubnet := testVPCBuildVPC(t, dbSession, "test-vpc-with-subnet", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "west4"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpcWithSubnet)
	assert.NotNil(t, testVPCBuildSubnet(t, dbSession, "test-subnet", tn, vpcWithSubnet, tnu))

	vpcWithInstance := testVPCBuildVPC(t, dbSession, "test-vpc-with-instance", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "west5"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpcWithInstance)
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type", st, cdbm.InstanceStatusReady)
	assert.NotNil(t, instanceType)
	allocationConstraint := testInstanceSiteBuildAllocationContraints(t, dbSession, al, cdbm.AllocationResourceTypeInstanceType, instanceType.ID, cdbm.AllocationConstraintTypeReserved, 1, tnu)
	assert.NotNil(t, allocationConstraint)
	assert.NotNil(t, testInstanceBuildInstance(t, dbSession, "test-instance", tn.ID, ip.ID, st.ID, &instanceType.ID, vpcWithInstance.ID, nil, nil, nil, cdbm.InstanceStatusReady))

	// Site not Registered — native networking enabled so failure is due to status, not FNN config
	stPending := testVPCBuildSite(t, dbSession, ip, "test-site-pending-reg", true, true, cdbm.SiteStatusPending, ipu)
	assert.NotNil(t, stPending)
	tsPending := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, stPending.ID, tnu.ID)
	assert.NotNil(t, tsPending)
	_ = testVPCSiteBuildAllocation(t, dbSession, stPending, tn, "test-allocation-pending-site", ipu)
	vpcPendingSite := testVPCBuildVPC(t, dbSession, "test-vpc-pending-site", ip, tn, stPending, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "west-pending"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpcPendingSite)

	// Registered site without native networking (FNN not enabled at site) — eligible otherwise (no subnets / instances)
	stNoNativeNet := testVPCBuildSite(t, dbSession, ip, "test-site-no-native-net", false, true, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, stNoNativeNet)
	tsNoNative := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, stNoNativeNet.ID, tnu.ID)
	assert.NotNil(t, tsNoNative)
	alNoNative := testVPCSiteBuildAllocation(t, dbSession, stNoNativeNet, tn, "test-allocation-no-native", ipu)
	assert.NotNil(t, alNoNative)
	vpcNoNativeNet := testVPCBuildVPC(t, dbSession, "test-vpc-no-native-net", ip, tn, stNoNativeNet, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "west-nonative"}, cdbm.VpcStatusReady, tnu)
	assert.NotNil(t, vpcNoNativeNet)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock per-Site client for st3
	tsc := &tmocks.Client{}
	tst := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st.ID.String()] = tsc
	scp.IDClientMap[st2.ID.String()] = tst

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateVPCVirtualization", mock.Anything).Return(wrun, nil)

	// Mock timeout error
	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")

	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tst.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateVPCVirtualization", mock.Anything).Return(wruntimeout, nil)

	tst.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "test VPC virtualization update success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcVirtualizationUpdateRequest{
					NetworkVirtualizationType: "FNN",
				},
				reqVPCID: vpc.ID.String(),
				reqVPC:   vpc,
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusOK,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC virtualization update error, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcVirtualizationUpdateRequest{
					NetworkVirtualizationType: "FNN",
				},
				reqVPCID: vpc.ID.String(),
				reqVPC:   vpc,
				reqOrg:   ipOrg,
				reqUser:  ipu,
				respCode: http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test VPC virtualization update error, invalid VPC ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcVirtualizationUpdateRequest{
					NetworkVirtualizationType: "FNN",
				},
				reqVPC:   vpc,
				reqVPCID: "",
				reqOrg:   tnOrg,
				reqUser:  tnu,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test VPC virtualization update API endpoint fail, workflow timeout",
			fields: fields{
				dbSession: dbSession,
				tc:        tst,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcVirtualizationUpdateRequest{
					NetworkVirtualizationType: "FNN",
				},
				reqOrg:      tnOrg,
				reqVPCID:    vpc3.ID.String(),
				reqVPC:      vpc3,
				reqUser:     tnu,
				respCode:    http.StatusInternalServerError,
				respMessage: "Failed to perform VPC UpdateVirtualization - timeout occurred executing workflow on Site",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC virtualization update API endpoint fail, vpc already FNN",
			fields: fields{
				dbSession: dbSession,
				tc:        tst,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcVirtualizationUpdateRequest{
					NetworkVirtualizationType: "FNN",
				},
				reqOrg:      tnOrg,
				reqVPCID:    vpc4.ID.String(),
				reqVPC:      vpc4,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "VPC virtualization type is already set to FNN",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC virtualization update API endpoint fail when VPC has subnets",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcVirtualizationUpdateRequest{
					NetworkVirtualizationType: "FNN",
				},
				reqOrg:      tnOrg,
				reqVPCID:    vpcWithSubnet.ID.String(),
				reqVPC:      vpcWithSubnet,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "Virtualization Type cannot be changed while VPC contains one or more Subnets",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC virtualization update API endpoint fail when VPC has instances",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcVirtualizationUpdateRequest{
					NetworkVirtualizationType: "FNN",
				},
				reqOrg:      tnOrg,
				reqVPCID:    vpcWithInstance.ID.String(),
				reqVPC:      vpcWithInstance,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "Virtualization Type cannot be changed while VPC contains one or more Instances",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC virtualization update API endpoint fail when Site is not Registered",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcVirtualizationUpdateRequest{
					NetworkVirtualizationType: "FNN",
				},
				reqOrg:      tnOrg,
				reqVPCID:    vpcPendingSite.ID.String(),
				reqVPC:      vpcPendingSite,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "Site that VPC belongs to must be in Registered state in order to update virtualization type",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC virtualization update API endpoint fail when Site does not have native networking enabled for FNN",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIVpcVirtualizationUpdateRequest{
					NetworkVirtualizationType: "FNN",
				},
				reqOrg:      tnOrg,
				reqVPCID:    vpcNoNativeNet.ID.String(),
				reqVPC:      vpcNoNativeNet,
				reqUser:     tnu,
				respCode:    http.StatusBadRequest,
				respMessage: "Site that VPC belongs to does not have native networking enabled, unable to update virtualization type to FNN",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			uvvh := UpdateVPCVirtualizationHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       scp,
				cfg:       tt.fields.cfg,
			}

			jsonData, _ := json.Marshal(tt.args.reqData)

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(string(jsonData)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/vpc/%v", tt.args.reqOrg, tt.args.reqVPCID))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.reqOrg, tt.args.reqVPCID)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := uvvh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("UpdateVPCVirtualizationHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("UpdateVPCVirtualizationHandler.Handle() resp = %v", rec.Body.String())
			}

			if tt.args.respMessage != "" {
				assert.Contains(t, rec.Body.String(), tt.args.respMessage)
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respCode != http.StatusOK {
				return
			}

			rst := &model.APIVpc{}

			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, *rst.NetworkVirtualizationType, tt.args.reqData.NetworkVirtualizationType)

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGetVPCHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		reqOrg   string
		reqUser  *cdbm.User
		reqVPC   *cdbm.Vpc
		reqVPCID string
		respCode int
	}

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	testVPCSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles := []string{authz.TenantAdminRole}

	ipu := testVPCBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	tnu1 := testVPCBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg1, tnOrgRoles)
	tn1 := testVPCBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)

	tnu2 := testVPCBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles)
	tn2 := testVPCBuildTenant(t, dbSession, "test-tenant-1", tnOrg2, tnu2)
	assert.NotNil(t, tn2)
	_ = common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip, &tn1.ID, tnOrg1, cdbm.TenantAccountStatusReady, tnu1)

	st := testVPCBuildSite(t, dbSession, ip, "test-site-1", false, true, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st)

	al := testVPCSiteBuildAllocation(t, dbSession, st, tn1, "test-allocation", ipu)
	assert.NotNil(t, al)

	vpc := testVPCBuildVPC(t, dbSession, "test-vpc", ip, tn1, st, cutil.GetPtr(cdbm.VpcFNN), nil, map[string]string{"zone": "west1"}, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc)
	vpc.RoutingProfileOverrides = &cdbm.VpcRoutingProfileOverrides{LeakDefaultRouteFromUnderlay: cutil.GetPtr(false)}
	vpc.EffectiveRoutingProfile = &cdbm.VpcEffectiveRoutingProfile{LeakDefaultRouteFromUnderlay: true, Internal: true, AccessTier: 5}
	testUpdateVPC(t, dbSession, vpc)

	// The second tenant has no targeted-instance-creation account and must not see effective state.
	unprivilegedVpc := testVPCBuildVPC(t, dbSession, "test-vpc-unprivileged", ip, tn2, st, cutil.GetPtr(cdbm.VpcFNN), nil, nil, cdbm.VpcStatusReady, tnu2)
	unprivilegedVpc.EffectiveRoutingProfile = &cdbm.VpcEffectiveRoutingProfile{Internal: true, AccessTier: 5}
	testUpdateVPC(t, dbSession, unprivilegedVpc)

	// Attach an NSG to this instance
	nsg1 := testBuildNetworkSecurityGroup(t, dbSession, "network-security-group-1-for-the-win", tn1, st, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg1)

	vpc.NetworkSecurityGroupID = cutil.GetPtr(nsg1.ID)
	testUpdateVPC(t, dbSession, vpc)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                             string
		fields                           fields
		args                             args
		wantErr                          bool
		queryIncludeRelations1           *string
		queryIncludeRelations2           *string
		expectedTenantOrg                *string
		expectedSiteName                 *string
		expectedNetworkSecurityGroupName *string
		expectEffectiveRoutingProfile    bool
		verifyChildSpanner               bool
	}{
		{
			name: "test VPC get API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc,
				reqVPCID: vpc.ID.String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusOK,
			},
			wantErr:                       false,
			expectEffectiveRoutingProfile: true,
		},
		// A tenant without effective site privilege receives no resolved profile field.
		{
			name: "test VPC get omits effective routing profile for unprivileged tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   unprivilegedVpc,
				reqVPCID: unprivilegedVpc.ID.String(),
				reqOrg:   tnOrg2,
				reqUser:  tnu2,
				respCode: http.StatusOK,
			},
			wantErr: false,
		},
		{
			name: "test VPC get API endpoint failure, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc,
				reqVPCID: vpc.ID.String(),
				reqOrg:   ipOrg,
				reqUser:  ipu,
				respCode: http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test VPC get API endpoint failure, invalid VPC ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc,
				reqVPCID: "",
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test VPC get API endpoint failure, VPC ID in request not found",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc,
				reqVPCID: uuid.New().String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusNotFound,
			},
			wantErr: false,
		},
		{
			name: "test VPC get API endpoint failure, VPC not belong to current tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc,
				reqVPCID: vpc.ID.String(),
				reqOrg:   tnOrg2,
				reqUser:  tnu2,
				respCode: http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test VPC get API endpoint success include tenant relation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc,
				reqVPCID: vpc.ID.String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusOK,
			},
			queryIncludeRelations1:        cutil.GetPtr(cdbm.TenantRelationName),
			expectedTenantOrg:             &tn1.Org,
			wantErr:                       false,
			expectEffectiveRoutingProfile: true,
			verifyChildSpanner:            true,
		},
		{
			name: "test VPC get API endpoint success include NSG relation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc,
				reqVPCID: vpc.ID.String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusOK,
			},
			queryIncludeRelations1:           cutil.GetPtr(cdbm.NetworkSecurityGroupRelationName),
			expectedNetworkSecurityGroupName: &nsg1.Name,
			wantErr:                          false,
			expectEffectiveRoutingProfile:    true,
			verifyChildSpanner:               true,
		},
		{
			name: "test VPC get API endpoint success include tenant/site relation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc,
				reqVPCID: vpc.ID.String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusOK,
			},
			queryIncludeRelations1:        cutil.GetPtr(cdbm.TenantRelationName),
			queryIncludeRelations2:        cutil.GetPtr(cdbm.SiteRelationName),
			expectedTenantOrg:             &tn1.Org,
			expectedSiteName:              &st.Name,
			wantErr:                       false,
			expectEffectiveRoutingProfile: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			csh := GetVPCHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			q := url.Values{}
			if tt.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tt.queryIncludeRelations1)
			}
			if tt.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tt.queryIncludeRelations2)
			}

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)

			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/vpc/%v", tt.args.reqOrg, tt.args.reqVPCID))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.reqOrg, tt.args.reqVPCID)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("GetVPCHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("GetVPCHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respCode != http.StatusOK {
				return
			}

			rst := &model.APIVpc{}

			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, rst.Name, tt.args.reqVPC.Name)
			assert.Equal(t, rst.Description, tt.args.reqVPC.Description)
			assert.Equal(t, tt.expectEffectiveRoutingProfile, rst.EffectiveRoutingProfile != nil)
			assert.Equal(t, tt.expectEffectiveRoutingProfile, strings.Contains(rec.Body.String(), "effectiveRoutingProfile"))
			if tt.args.reqVPC.RoutingProfileOverrides != nil {
				assert.NotNil(t, rst.RoutingProfileOverrides)
			}

			if tt.expectedTenantOrg != nil {
				assert.Equal(t, rst.Tenant.Org, *tt.expectedTenantOrg)
			}

			if tt.expectedNetworkSecurityGroupName != nil {
				assert.Equal(t, rst.NetworkSecurityGroup.Name, *tt.expectedNetworkSecurityGroupName)
			}

			if tt.expectedSiteName != nil {
				assert.Equal(t, rst.Site.Name, *tt.expectedSiteName)
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGetAllVPCHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	type args struct {
		org   string
		query url.Values
		user  *cdbm.User
	}

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	testVPCSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org"
	tnOrgRoles := []string{authz.TenantAdminRole}
	tn2Org := "test-tenant-org-2"

	ipu := testVPCBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	tnu := testVPCBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn := testVPCBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu)
	_ = common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip, &tn.ID, tnOrg, cdbm.TenantAccountStatusReady, tnu)
	tnu2 := testVPCBuildUser(t, dbSession, "test-starfleet-id-3", tn2Org, tnOrgRoles)
	tn2 := testVPCBuildTenant(t, dbSession, "test-tenant-2", tn2Org, tnu2)

	st := testVPCBuildSite(t, dbSession, ip, "test-site-1", false, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st)

	al := testVPCSiteBuildAllocation(t, dbSession, st, tn, "test-allocation", ipu)
	assert.NotNil(t, al)

	st2 := testVPCBuildSite(t, dbSession, ip, "test-site-2", false, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st2)

	al2 := testVPCSiteBuildAllocation(t, dbSession, st2, tn, "test-allocation-2", ipu)
	assert.NotNil(t, al2)

	// A per-Site false override removes only Site 2 from the account-level privilege.
	tenantSite2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st2.ID, tnu.ID)
	_, err := cdbm.NewTenantSiteDAO(dbSession).Update(ctx, nil, cdbm.TenantSiteUpdateInput{
		TenantSiteID: tenantSite2.ID,
		Config:       &cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(false)},
	})
	require.NoError(t, err)

	// Site with no allocations for first tenant
	// We'll add VPCs to simulate a site where tenant had allocations
	// but they were deleted without deleting VPCs.
	st3 := testVPCBuildSite(t, dbSession, ip, "test-site-3", false, false, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st3)

	al3 := testVPCSiteBuildAllocation(t, dbSession, st3, tn2, "test-allocation-3", ipu)
	assert.NotNil(t, al3)

	// NSGs
	nsg1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg1", tn, st, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg1)
	nsg2 := testBuildNetworkSecurityGroup(t, dbSession, "nsg2", tn, st2, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg2)
	nsg3 := testBuildNetworkSecurityGroup(t, dbSession, "nsg3", tn, st3, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg3)
	nsg4 := testBuildNetworkSecurityGroup(t, dbSession, "nsg4", tn2, st3, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg4)

	nsgs := []*cdbm.NetworkSecurityGroup{nsg1, nsg2, nsg3}

	// NVLink Logical Partitions
	nvllp1 := testVPCBuildNVLinkLogicalPartition(t, dbSession, "nvllp1", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg, st, tn, cdbm.NVLinkLogicalPartitionStatusReady, tnu)
	assert.NotNil(t, nvllp1)
	nvllp2 := testVPCBuildNVLinkLogicalPartition(t, dbSession, "nvllp2", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg, st2, tn, cdbm.NVLinkLogicalPartitionStatusReady, tnu)
	assert.NotNil(t, nvllp2)
	nvllp3 := testVPCBuildNVLinkLogicalPartition(t, dbSession, "nvllp3", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg, st3, tn, cdbm.NVLinkLogicalPartitionStatusReady, tnu)
	assert.NotNil(t, nvllp3)
	nvllp4 := testVPCBuildNVLinkLogicalPartition(t, dbSession, "nvllp4", cutil.GetPtr("Test NVLink Logical Partition"), tn2Org, st3, tn2, cdbm.NVLinkLogicalPartitionStatusReady, tnu2)
	assert.NotNil(t, nvllp4)

	nvllps := []*cdbm.NVLinkLogicalPartition{nvllp1, nvllp2, nvllp3}

	sites := []*cdbm.Site{st, st2, st3}
	siteCount := len(sites)

	vpcsPerSite := 15

	// Total VPC count
	totalCount := siteCount * vpcsPerSite

	vpcs := []cdbm.Vpc{}

	for i := 0; i < totalCount; i++ {
		curSite := sites[i%siteCount]
		curNsg := nsgs[i%siteCount]
		curNvllp := nvllps[i%siteCount]

		var status string
		if i%2 == 0 {
			status = cdbm.VpcStatusReady
		} else {
			status = cdbm.VpcStatusPending
		}

		vpc := testVPCBuildVPC(t, dbSession, fmt.Sprintf("test-vpc-%02d", i), ip, tn, curSite, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), &curNvllp.ID, map[string]string{"zone": fmt.Sprintf("test-vpc-%02d", i)}, status, tnu)
		assert.NotNil(t, vpc)

		// Add the NSG of the site to the VPC
		vpc.NetworkSecurityGroupID = cutil.GetPtr(curNsg.ID)
		vpc.EffectiveRoutingProfile = &cdbm.VpcEffectiveRoutingProfile{Internal: true, AccessTier: uint32(i + 1)}
		testUpdateVPC(t, dbSession, vpc)

		vpcs = append(vpcs, *vpc)
	}

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                             string
		fields                           fields
		args                             args
		wantCount                        int
		wantTotalCount                   int
		wantFirstEntry                   *cdbm.Vpc
		wantRespCode                     int
		expectedTenantOrg                *string
		expectedNetworkSecurityGroupName *string
		expectedSiteName                 *string
		verifyChildSpanner               bool
	}{
		{
			name: "get all VPCs success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org:   tnOrg,
				query: url.Values{},
				user:  tnu,
			},
			wantCount:          paginator.DefaultLimit,
			wantTotalCount:     totalCount,
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "get all VPCs for matching Tenant ID success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"tenantId": []string{tn.ID.String()},
				},
				user: tnu,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs failure, invalid Tenant ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"tenantId": []string{"invalid-tenant-id"},
				},
				user: tnu,
			},
			wantRespCode: http.StatusBadRequest,
		},
		{
			name: "get all VPCs failure, Tenant ID belongs to another org",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"tenantId": []string{tn2.ID.String()},
				},
				user: tnu,
			},
			wantRespCode: http.StatusForbidden,
		},
		{
			name: "get all VPCs when no allocations, should pass",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"includeRelation": []string{cdbm.SiteRelationName},
					"siteId":          []string{st3.ID.String()},
				},
				user: tnu,
			},
			wantCount:        vpcsPerSite,
			wantTotalCount:   vpcsPerSite,
			wantRespCode:     http.StatusOK,
			expectedSiteName: &st3.Name,
		},
		{
			name: "get all VPCs with Site filter success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"siteId": []string{st.ID.String()},
				},
				user: tnu,
			},
			wantCount:      vpcsPerSite,
			wantTotalCount: vpcsPerSite,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs with multiple sites filter success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"siteId": []string{st.ID.String(), st2.ID.String()},
				},
				user: tnu,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: vpcsPerSite * 2,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs failure, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org:   ipOrg,
				query: url.Values{},
				user:  ipu,
			},
			wantRespCode: http.StatusForbidden,
		},
		{
			name: "get all VPCs failure, invalid Site ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"siteId": []string{"invalid-site-id"},
				},
				user: tnu,
			},
			wantRespCode: http.StatusBadRequest,
		},
		{
			name: "get all VPCs failure, non-existent Site ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"siteId": []string{uuid.New().String()},
				},
				user: tnu,
			},
			wantRespCode: http.StatusBadRequest,
		},
		{
			name: "get all VPCs with pagination success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"pageNumber": []string{"2"},
					"pageSize":   []string{"5"},
					"orderBy":    []string{"NAME_DESC"},
				},
				user: tnu,
			},
			wantCount:      5,
			wantTotalCount: totalCount,
			wantFirstEntry: &vpcs[39],
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs with pagination failure, invalid page size",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"pageNumber": []string{"1"},
					"pageSize":   []string{"200"},
					"orderBy":    []string{"NAME_ASC"},
				},
				user: tnu,
			},
			wantRespCode: http.StatusBadRequest,
		},
		{
			name: "get all VPCs with Site include relation success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"includeRelation": []string{cdbm.SiteRelationName, cdbm.NetworkSecurityGroupRelationName},
					"siteId":          []string{st.ID.String()},
				},
				user: tnu,
			},
			wantCount:                        vpcsPerSite,
			wantTotalCount:                   vpcsPerSite,
			wantRespCode:                     http.StatusOK,
			expectedSiteName:                 &st.Name,
			expectedNetworkSecurityGroupName: &nsg1.Name,
		},
		{
			name: "get all VPCs by name as query full text search success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"query": []string{"test-vpc-"},
				},
				user: tnu,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs by description as query full text search success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"query": []string{"Test VPC"},
				},
				user: tnu,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs by status as query full text search success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"query": []string{cdbm.VpcStatusPending},
				},
				user: tnu,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount / 2,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs by status as query full text search success returns no object",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"query": []string{cdbm.VpcStatusDeleting},
				},
				user: tnu,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs by combination of name and status as query full text search success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"query": []string{"test-vpc- pending"},
				},
				user: tnu,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs by VpcStatusPending status success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"status": []string{cdbm.VpcStatusPending},
				},
				user: tnu,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount / 2,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs by VpcStatusDeleting status success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"status": []string{cdbm.VpcStatusDeleting},
				},
				user: tnu,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs by multiple statuses success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"status": []string{cdbm.VpcStatusPending, cdbm.VpcStatusReady},
				},
				user: tnu,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs failure, BadStatus status value in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"status": []string{"BadStatus"},
				},
				user: tnu,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusBadRequest,
		},
		{
			name: "get all VPCs, filter by network security group ID success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"networkSecurityGroupId": []string{nsg1.ID},
				},
				user: tnu,
			},
			wantCount:      vpcsPerSite,
			wantTotalCount: vpcsPerSite,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs, filter by multiple network security group IDs success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"networkSecurityGroupId": []string{nsg1.ID, nsg2.ID},
				},
				user: tnu,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: vpcsPerSite * 2,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs, filter by network security group ID failure, network security group ID does not exist",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"networkSecurityGroupId": []string{uuid.NewString()},
				},
				user: tnu,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusBadRequest,
		},
		{
			name: "get all VPCs, filter by network security group belonging to different tenant failure",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"networkSecurityGroupId": []string{nsg4.ID},
				},
				user: tnu,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusBadRequest,
		},
		{
			name: "get all VPCs, filter by nvlink logical partition ID success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"nvLinkLogicalPartitionId": []string{nvllp1.ID.String()},
				},
				user: tnu,
			},
			wantCount:      vpcsPerSite,
			wantTotalCount: vpcsPerSite,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs, filter by multiple nvlink logical partition IDs success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"nvLinkLogicalPartitionId": []string{nvllp1.ID.String(), nvllp2.ID.String()},
				},
				user: tnu,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: vpcsPerSite * 2,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all VPCs, filter by nvlink logical partition ID failure, nvlink logical partition ID does not exist",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"nvLinkLogicalPartitionId": []string{uuid.NewString()},
				},
				user: tnu,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusBadRequest,
		},
		{
			name: "get all VPCs, filter by nvlink logical partition ID failure, nvlink logical partition ID is not owned by current tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				org: tnOrg,
				query: url.Values{
					"nvLinkLogicalPartitionId": []string{nvllp4.ID.String()},
				},
				user: tnu,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusBadRequest,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			csh := GetAllVPCHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			path := fmt.Sprintf("/v2/org/%s/nico/vpc?%s", tt.args.org, tt.args.query.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := csh.Handle(ec)
			require.NoError(t, err)

			assert.Equal(t, tt.wantRespCode, rec.Code)
			if tt.wantRespCode != http.StatusOK {
				return
			}

			resp := []model.APIVpc{}

			err = json.Unmarshal(rec.Body.Bytes(), &resp)
			require.NoError(t, err)

			assert.Equal(t, tt.wantCount, len(resp))

			// Bulk responses apply effective privilege independently for each VPC Site.
			for _, responseVpc := range resp {
				require.NotNil(t, responseVpc.SiteID)
				if *responseVpc.SiteID == st2.ID.String() {
					assert.Nil(t, responseVpc.EffectiveRoutingProfile)
				} else {
					assert.NotNil(t, responseVpc.EffectiveRoutingProfile)
				}
			}

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			require.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err = json.Unmarshal([]byte(ph), pr)
			require.NoError(t, err)

			assert.Equal(t, tt.wantTotalCount, pr.Total)

			if tt.wantFirstEntry != nil {
				assert.Equal(t, tt.wantFirstEntry.Name, resp[0].Name)
			}

			if len(resp) > 0 {
				if tt.expectedTenantOrg != nil {
					assert.Equal(t, resp[0].Tenant.Org, *tt.expectedTenantOrg)
				} else {
					assert.Nil(t, resp[0].Tenant)
				}
				if tt.expectedSiteName != nil {
					assert.Equal(t, resp[0].Site.Name, *tt.expectedSiteName)
				} else {
					assert.Nil(t, resp[0].Site)
				}

				for _, apivpc := range resp {
					if tt.expectedNetworkSecurityGroupName != nil {
						assert.NotNil(t, apivpc.NetworkSecurityGroupID, "NetworkSecurityGroupID for VPC in api response was unexpectedly nil.  Did you forget to set it for this test?")
						assert.NotNil(t, apivpc.NetworkSecurityGroup, "NetworkSecurityGroup for VPC in api response was unexpectedly nil.  Did you forget to include the relation for this test?")
						assert.Equal(t, *tt.expectedNetworkSecurityGroupName, apivpc.NetworkSecurityGroup.Name)
					}
					assert.Equal(t, 0, len(apivpc.StatusHistory))
				}
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestDeleteVPCHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
		scp       *sc.ClientPool
	}
	type args struct {
		reqOrg   string
		reqUser  *cdbm.User
		reqVPC   string
		respCode int
	}

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	testVPCSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles := []string{authz.TenantAdminRole}

	ipu := testVPCBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	tnu1 := testVPCBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg1, tnOrgRoles)
	tn1 := testVPCBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnu1)

	tnu2 := testVPCBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles)
	tn2 := testVPCBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, tnu1)
	assert.NotNil(t, tn2)

	st := testVPCBuildSite(t, dbSession, ip, "test-site-1", false, true, cdbm.SiteStatusRegistered, ipu)
	assert.NotNil(t, st)

	al := testVPCSiteBuildAllocation(t, dbSession, st, tn1, "test-allocation", ipu)
	assert.NotNil(t, al)

	ipb1 := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb1", st, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "10.0.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
	assert.NotNil(t, ipb1)

	// NVLink Logical Partition for tenant 1 on site 1
	nvllp1 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-1", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg1, st, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp1)

	vpc1 := testVPCBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(nvllp1.ID), map[string]string{"zone": "east1"}, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	vpc2 := testVPCBuildVPC(t, dbSession, "test-vpc-2", ip, tn1, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, map[string]string{"zone": "east1"}, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc2)

	vpc3 := testVPCBuildVPC(t, dbSession, "test-vpc-3", ip, tn1, st, cutil.GetPtr(cdbm.VpcFNN), nil, map[string]string{"zone": "east1"}, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc3)

	os := common.TestBuildOperatingSystem(t, dbSession, "test-os", tn1, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os)

	it := common.TestBuildInstanceType(t, dbSession, "testIT", cutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "test-instance-type-1",
		"description": "Test Instance Type 1 Description",
	}, ipu)
	assert.NotNil(t, it)

	machine := common.TestBuildMachine(t, dbSession, ip, st, &it.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)
	assert.NotNil(t, machine)

	alc := common.TestBuildAllocationConstraint(t, dbSession, al, it, nil, 1, ipu)
	assert.NotNil(t, alc)

	instance := common.TestBuildInstance(t, dbSession, "test-instance", tn1.ID, ip.ID, st.ID, it.ID, vpc3.ID, &machine.ID, os.ID)
	assert.NotNil(t, instance)

	subnet := testVPCBuildSubnet(t, dbSession, "test-subnet", tn1, vpc2, tnu1)
	assert.NotNil(t, subnet)

	vpcPrefix := testVPCBuildVPCPrefix(t, dbSession, "test-vpc-prefix", tn1, vpc3, cutil.GetPtr(ipb1.ID), "10.0.0.0/24", tnu1)
	assert.NotNil(t, vpcPrefix)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp", cutil.GetPtr("Test NVLink Logical Partition"), tn1.Org, st, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}
	tcfg, _ := cfg.GetTemporalConfig()

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[st.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteVPCV2", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo not-found mocking
	//
	scpWithNICoNotFound := sc.NewClientPool(tcfg)
	tscWithNICoNotFound := &tmocks.Client{}

	scpWithNICoNotFound.IDClientMap[st.ID.String()] = tscWithNICoNotFound

	wrunWithNICoNotFound := &tmocks.WorkflowRun{}
	wrunWithNICoNotFound.On("GetID").Return("workflow-WithNICoNotFound")

	wrunWithNICoNotFound.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoObjectNotFound, errors.New("NICo went bananas")))

	tscWithNICoNotFound.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteVPCV2", mock.Anything).Return(wrunWithNICoNotFound, nil)

	tscWithNICoNotFound.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// Prepare client pool for sync calls
	// to site(s).

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteVPCV2", mock.Anything).Return(wrun, nil)

	// OTEL Spanner configurations
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "test VPC delete API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc1.ID.String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusAccepted,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC delete API endpoint nico not-found, still success",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithNICoNotFound,
				scp:       scpWithNICoNotFound,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc1.ID.String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusAccepted,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC delete API endpoint workflow timeout failure",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithTimeout,
				scp:       scpWithTimeout,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc1.ID.String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusInternalServerError,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test VPC delete API endpoint failure, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc1.ID.String(),
				reqOrg:   ipOrg,
				reqUser:  ipu,
				respCode: http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test VPC delete API endpoint failure, invalid VPC ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   "",
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test VPC delete API endpoint failure, VPC not found",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   uuid.New().String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusNotFound,
			},
			wantErr: false,
		},
		{
			name: "test VPC delete API endpoint failure, VPC not belong to current tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc1.ID.String(),
				reqOrg:   tnOrg2,
				reqUser:  tnu2,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test VPC delete API endpoint failure, VPC has subnet attached",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc2.ID.String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test VPC delete API endpoint failure, VPC has VPC prefix attached",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc3.ID.String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test VPC delete API endpoint failure, VPC has instance attached",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqVPC:   vpc3.ID.String(),
				reqOrg:   tnOrg1,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			csh := DeleteVPCHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       tt.fields.scp,
				cfg:       tt.fields.cfg,
			}

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodDelete, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/vpc/%v", tt.args.reqOrg, tt.args.reqVPC))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.reqOrg, tt.args.reqVPC)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("DeleteVPCHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("DeleteVPCHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respCode != http.StatusAccepted {
				return
			}
			assertDeletionAcceptedResponse(t, rec.Body.Bytes())

			// Verify VPC in deleting state
			vpcDAO := cdbm.NewVpcDAO(dbSession)
			vpcID, _ := uuid.Parse(tt.args.reqVPC)
			dvpc, terr := vpcDAO.GetByID(context.Background(), nil, vpcID, nil)
			assert.Nil(t, terr)
			assert.Equal(t, cdbm.VpcStatusDeleting, dvpc.Status)

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNewCreateVPCHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testVPCInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	tests := []struct {
		name string
		args args
		want CreateVPCHandler
	}{
		{
			name: "test CreateVPCHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: CreateVPCHandler{
				dbSession:  dbSession,
				tc:         tc,
				scp:        scp,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewCreateVPCHandler(tt.args.dbSession, tt.args.tc, scp, tt.args.cfg, nil)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestNewUpdateVPCHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testVPCInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	tests := []struct {
		name string
		args args
		want UpdateVPCHandler
	}{
		{
			name: "test UpdateVPCHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: UpdateVPCHandler{
				dbSession:  dbSession,
				tc:         tc,
				scp:        scp,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewUpdateVPCHandler(tt.args.dbSession, tt.args.tc, scp, tt.args.cfg, nil)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestNewGetVPCHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testVPCInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want GetVPCHandler
	}{
		{
			name: "test GetVPCHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetVPCHandler{
				dbSession:  dbSession,
				tc:         tc,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetVPCHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewGetVPCHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNewGetAllVPCHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testVPCInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want GetAllVPCHandler
	}{
		{
			name: "test GetAllVPCHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetAllVPCHandler{
				dbSession:  dbSession,
				tc:         tc,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetAllVPCHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewGetAllVPCHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNewDeleteVPCHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testVPCInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	tests := []struct {
		name string
		args args
		want DeleteVPCHandler
	}{
		{
			name: "test DeleteVPCHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: DeleteVPCHandler{
				dbSession:  dbSession,
				tc:         tc,
				cfg:        cfg,
				scp:        scp,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewDeleteVPCHandler(tt.args.dbSession, tt.args.tc, scp, tt.args.cfg, nil)
			assert.Equal(t, tt.want, got)
		})
	}
}
