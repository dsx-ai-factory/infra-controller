// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
	"google.golang.org/protobuf/proto"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/extra/bundebug"
)

func testInstanceInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

// reset the tables needed for Instance tests
func testInstanceSetupSchema(t *testing.T, dbSession *db.Session) {
	// create Allocation table
	err := dbSession.DB.ResetModel(context.Background(), (*Allocation)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*InstanceType)(nil))
	assert.Nil(t, err)
	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*Vpc)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*IPBlock)(nil))
	assert.Nil(t, err)
	// create VpcPrefix table
	err = dbSession.DB.ResetModel(context.Background(), (*VpcPrefix)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*Machine)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*MachineCapability)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*OperatingSystem)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*Instance)(nil))
	assert.Nil(t, err)
	// create Interface table
	err = dbSession.DB.ResetModel(context.Background(), (*Interface)(nil))
	assert.Nil(t, err)
	// create SSHKey table
	err = dbSession.DB.ResetModel(context.Background(), (*SSHKey)(nil))
	assert.Nil(t, err)
	// create SSHKeyGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*SSHKeyGroup)(nil))
	assert.Nil(t, err)
}

func testInstanceBuildInfrastructureProvider(t *testing.T, dbSession *db.Session, name string) *InfrastructureProvider {
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        name,
		DisplayName: cutil.GetPtr("TestInfraProvider"),
		Org:         "test",
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	assert.Nil(t, err)
	return ip
}

func testInstanceBuildSite(t *testing.T, dbSession *db.Session, ip *InfrastructureProvider, name string) *Site {
	st := &Site{
		ID:                          uuid.New(),
		Name:                        name,
		DisplayName:                 cutil.GetPtr("Test"),
		Org:                         "test",
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(db.GetCurTime()),
		Status:                      SiteStatusPending,
		CreatedBy:                   uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	assert.Nil(t, err)
	return st
}

func testInstanceBuildTenant(t *testing.T, dbSession *db.Session, name string) *Tenant {
	tenant := &Tenant{
		ID:             uuid.New(),
		Name:           name,
		Org:            "test",
		OrgDisplayName: cutil.GetPtr(name + "-display"),
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

func testInstanceBuildVpc(t *testing.T, dbSession *db.Session, infrastructureProvider *InfrastructureProvider, site *Site, tenant *Tenant, name string) *Vpc {
	vpc := &Vpc{
		ID:                       uuid.New(),
		Name:                     name,
		Org:                      "test",
		InfrastructureProviderID: infrastructureProvider.ID,
		SiteID:                   site.ID,
		TenantID:                 tenant.ID,
		Status:                   VpcStatusPending,
		CreatedBy:                uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(vpc).Exec(context.Background())
	assert.Nil(t, err)
	return vpc
}

func testInstanceBuildSubnet(t *testing.T, dbSession *db.Session, tenant *Tenant, vpc *Vpc, name string) *Subnet {
	subnet := &Subnet{
		ID:        uuid.New(),
		Name:      name,
		SiteID:    vpc.SiteID,
		VpcID:     vpc.ID,
		TenantID:  tenant.ID,
		Status:    SubnetStatusPending,
		CreatedBy: uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(subnet).Exec(context.Background())
	assert.Nil(t, err)
	return subnet
}

func testInstanceBuildInstanceType(t *testing.T, dbSession *db.Session, ip *InfrastructureProvider, name string) *InstanceType {
	instanceType := &InstanceType{
		ID:                       uuid.New(),
		Name:                     name,
		InfrastructureProviderID: ip.ID,
		Status:                   InstanceTypeStatusPending,
	}
	_, err := dbSession.DB.NewInsert().Model(instanceType).Exec(context.Background())
	assert.Nil(t, err)
	return instanceType
}

func testInstanceBuildNetworkSecurityGroup(t *testing.T, dbSession *db.Session, tenant *Tenant, site *Site, name string) *NetworkSecurityGroup {
	networkSecurityGroup := &NetworkSecurityGroup{
		ID:        uuid.NewString(),
		Name:      name,
		SiteID:    site.ID,
		TenantOrg: tenant.Org,
		TenantID:  tenant.ID,
		Status:    InstanceTypeStatusReady,
		Rules: []*NetworkSecurityGroupRule{
			&NetworkSecurityGroupRule{
				&corev1.NetworkSecurityGroupRuleAttributes{
					Id:     cutil.GetPtr(uuid.NewString()),
					Action: corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_DENY,
				},
			},
		},
	}

	_, err := dbSession.DB.NewInsert().Model(networkSecurityGroup).Exec(context.Background())
	assert.Nil(t, err)
	return networkSecurityGroup
}

func testInstanceBuildAllocation(t *testing.T, dbSession *db.Session, ip *InfrastructureProvider, tenant *Tenant, site *Site, name string) *Allocation {
	allocation := &Allocation{
		ID:                       uuid.New(),
		Name:                     name,
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant.ID,
		SiteID:                   site.ID,
		Status:                   AllocationStatusPending,
		CreatedBy:                uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(allocation).Exec(context.Background())
	assert.Nil(t, err)
	return allocation
}

func testInstanceBuildOperatingSystem(t *testing.T, dbSession *db.Session, name string) *OperatingSystem {
	operatingSystem := &OperatingSystem{
		ID:        uuid.New(),
		Name:      name,
		Status:    OperatingSystemStatusPending,
		CreatedBy: uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(operatingSystem).Exec(context.Background())
	assert.Nil(t, err)
	return operatingSystem
}

func testInstanceBuildUser(t *testing.T, dbSession *db.Session, starfleetID string) *User {
	user := &User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(starfleetID),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
	}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	assert.Nil(t, err)
	return user
}

func TestInstanceSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant, site, "testNetworkSecurityGroup")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	ossd := NewInstanceDAO(dbSession)
	dummyUUID := uuid.New()
	dummyMachineID := uuid.NewString()

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		is                 []Instance
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			is: []Instance{
				{
					Name:                     "test",
					Description:              cutil.GetPtr("Test description"),
					TenantID:                 tenant.ID,
					InfrastructureProviderID: ip.ID,
					SiteID:                   site.ID,
					InstanceTypeID:           &instanceType.ID,
					NetworkSecurityGroupID:   &networkSecurityGroup.ID,
					NetworkSecurityGroupPropagationDetails: &NetworkSecurityGroupPropagationDetails{
						NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{
							Id:                      "",
							RelatedInstanceIds:      []string{},
							UnpropagatedInstanceIds: []string{},
						},
					},
					VpcID:                    vpc.ID,
					MachineID:                &machine.ID,
					Hostname:                 cutil.GetPtr("test.com"),
					OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
					Status:                   InstanceStatusPending,
					PowerStatus:              cutil.GetPtr(InstancePowerStatusBootCompleted),
					IpxeScript:               cutil.GetPtr("ipxe"),
					AlwaysBootWithCustomIpxe: true,
					PhoneHomeEnabled:         true,
					UserData:                 cutil.GetPtr("data"),
					CreatedBy:                user.ID,
					Labels:                   map[string]string{},
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple, some with null fields",
			is: []Instance{
				{
					Name: "test1", Description: cutil.GetPtr("Test description"), TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site.ID, InstanceTypeID: &instanceType.ID, VpcID: vpc.ID, MachineID: &machine.ID, Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID), Status: InstanceStatusPending, IpxeScript: cutil.GetPtr("ipxe"), AlwaysBootWithCustomIpxe: true, PhoneHomeEnabled: true, UserData: cutil.GetPtr("data"), IsUpdatePending: true, CreatedBy: user.ID, Labels: map[string]string{},
				},
				{
					Name: "test2", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site.ID, InstanceTypeID: &instanceType.ID, VpcID: vpc.ID, MachineID: &machine.ID, Hostname: nil, OperatingSystemID: cutil.GetPtr(operatingSystem.ID), InfinityRCRStatus: cutil.GetPtr("RESOURCE_GRANTED"), Status: InstanceStatusPending, IpxeScript: nil, PhoneHomeEnabled: false, UserData: nil, CreatedBy: user.ID, Labels: map[string]string{},
				},
				{
					Name: "test3", Description: cutil.GetPtr("Test description 3"), TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site.ID, InstanceTypeID: &instanceType.ID, VpcID: vpc.ID, MachineID: &machine.ID, Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID), InfinityRCRStatus: cutil.GetPtr("RESOURCE_GRANTED"), Status: InstanceStatusPending, IpxeScript: cutil.GetPtr("ipxe"), AlwaysBootWithCustomIpxe: true, UserData: cutil.GetPtr("data"), CreatedBy: user.ID, Labels: map[string]string{},
				},
			},
			expectError: false,
		},
		{
			desc: "failure - foreign key violation on tenant_id",
			is: []Instance{
				{
					Name: "test", TenantID: dummyUUID, InfrastructureProviderID: ip.ID, SiteID: site.ID, InstanceTypeID: &instanceType.ID, VpcID: vpc.ID, MachineID: &machine.ID, Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID), Status: InstanceStatusPending, IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("data"), CreatedBy: user.ID, Labels: map[string]string{},
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on infrastructure_provider_id",
			is: []Instance{
				{
					Name: "test", TenantID: tenant.ID, InfrastructureProviderID: dummyUUID, SiteID: site.ID, InstanceTypeID: &instanceType.ID, VpcID: vpc.ID, MachineID: &machine.ID, Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID), Status: InstanceStatusPending, IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("data"), CreatedBy: user.ID, Labels: map[string]string{},
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on site_id",
			is: []Instance{
				{
					Name: "test", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: dummyUUID, InstanceTypeID: &instanceType.ID, VpcID: vpc.ID, MachineID: &machine.ID, Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID), Status: InstanceStatusPending, IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("data"), CreatedBy: user.ID, Labels: map[string]string{},
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on instance_type_id",
			is: []Instance{
				{
					Name: "test", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site.ID, InstanceTypeID: &dummyUUID, VpcID: vpc.ID, MachineID: &machine.ID, Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID), Status: InstanceStatusPending, IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("data"), CreatedBy: user.ID, Labels: map[string]string{},
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on vpc_id",
			is: []Instance{
				{
					Name: "test", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site.ID, InstanceTypeID: &instanceType.ID, VpcID: dummyUUID, MachineID: &machine.ID, Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID), Status: InstanceStatusPending, IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("data"), CreatedBy: user.ID, Labels: map[string]string{},
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on machine_id",
			is: []Instance{
				{
					Name: "test", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site.ID, InstanceTypeID: &instanceType.ID, VpcID: vpc.ID, MachineID: &dummyMachineID, Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID), Status: InstanceStatusPending, IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("data"), CreatedBy: user.ID, Labels: map[string]string{},
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on operating_system_id",
			is: []Instance{
				{
					Name: "test", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site.ID, InstanceTypeID: &instanceType.ID, VpcID: vpc.ID, MachineID: &machine.ID, Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(dummyUUID), Status: InstanceStatusPending, IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("data"), CreatedBy: user.ID, Labels: map[string]string{},
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on network_security_group_id",
			is: []Instance{
				{
					Name: "test", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site.ID, InstanceTypeID: &instanceType.ID, NetworkSecurityGroupID: cutil.GetPtr(uuid.NewString()), VpcID: vpc.ID, MachineID: &machine.ID, Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(dummyUUID), Status: InstanceStatusPending, IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("data"), CreatedBy: user.ID, Labels: map[string]string{},
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.is {
				got, err := ossd.Create(
					ctx, nil,
					InstanceCreateInput{
						Name:                                   i.Name,
						Description:                            i.Description,
						TenantID:                               i.TenantID,
						InfrastructureProviderID:               i.InfrastructureProviderID,
						SiteID:                                 i.SiteID,
						InstanceTypeID:                         i.InstanceTypeID,
						NetworkSecurityGroupID:                 i.NetworkSecurityGroupID,
						NetworkSecurityGroupPropagationDetails: i.NetworkSecurityGroupPropagationDetails,
						VpcID:                                  i.VpcID,
						MachineID:                              i.MachineID,
						Hostname:                               i.Hostname,
						OperatingSystemID:                      i.OperatingSystemID,
						IpxeScript:                             i.IpxeScript,
						AlwaysBootWithCustomIpxe:               i.AlwaysBootWithCustomIpxe,
						PhoneHomeEnabled:                       i.PhoneHomeEnabled,
						UserData:                               i.UserData,
						Labels:                                 i.Labels,
						IsUpdatePending:                        i.IsUpdatePending,
						InfinityRCRStatus:                      i.InfinityRCRStatus,
						Status:                                 i.Status,
						PowerStatus:                            i.PowerStatus,
						CreatedBy:                              i.CreatedBy,
					},
				)
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, got)
				}
				if i.NetworkSecurityGroupPropagationDetails != nil {
					assert.True(t, proto.Equal(i.NetworkSecurityGroupPropagationDetails, got.NetworkSecurityGroupPropagationDetails))
				}

				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
					_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
					assert.True(t, ok)
				}
			}
		})
	}
}

func TestInstanceSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant, site, "testNetworkSecurityGroup")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)
	dummyUUID := uuid.New()
	i1, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			NetworkSecurityGroupID:   &networkSecurityGroup.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			PhoneHomeEnabled:         true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                           string
		id                             uuid.UUID
		instance                       *Instance
		paramRelations                 []string
		expectedError                  bool
		expectedErrVal                 error
		expectedTenant                 bool
		expectedInfrastructureProvider bool
		expectedSite                   bool
		expectedInstanceType           bool
		expectedNetworkSecurityGroup   bool
		expectedVpc                    bool
		expectedMachine                bool
		expectedOperatingSystem        bool
		verifyChildSpanner             bool
	}{
		{
			desc:                           "GetById success when Instance exists",
			id:                             i1.ID,
			instance:                       i1,
			paramRelations:                 []string{},
			expectedError:                  false,
			expectedTenant:                 false,
			expectedInfrastructureProvider: false,
			expectedSite:                   false,
			expectedInstanceType:           false,
			expectedVpc:                    false,
			expectedMachine:                false,
			expectedOperatingSystem:        false,
			verifyChildSpanner:             true,
		},
		{
			desc:                           "GetById error when not found",
			id:                             dummyUUID,
			instance:                       i1,
			paramRelations:                 []string{},
			expectedError:                  true,
			expectedErrVal:                 db.ErrDoesNotExist,
			expectedTenant:                 false,
			expectedInfrastructureProvider: false,
			expectedSite:                   false,
			expectedInstanceType:           false,
			expectedVpc:                    false,
			expectedMachine:                false,
			expectedOperatingSystem:        false,
		},
		{
			desc:                           "GetById with supported relations",
			id:                             i1.ID,
			instance:                       i1,
			paramRelations:                 []string{TenantRelationName, InfrastructureProviderRelationName, SiteRelationName, InstanceTypeRelationName, VpcRelationName, MachineRelationName, OperatingSystemRelationName, NetworkSecurityGroupRelationName},
			expectedError:                  false,
			expectedTenant:                 true,
			expectedInfrastructureProvider: true,
			expectedSite:                   true,
			expectedInstanceType:           true,
			expectedNetworkSecurityGroup:   true,
			expectedVpc:                    true,
			expectedMachine:                true,
			expectedOperatingSystem:        true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := isd.GetByID(ctx, nil, tc.id, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.EqualValues(t, tc.instance.ID, got.ID)
				if tc.expectedTenant {
					assert.EqualValues(t, tc.instance.TenantID, got.Tenant.ID)
				}
				if tc.expectedInfrastructureProvider {
					assert.EqualValues(t, tc.instance.InfrastructureProviderID, got.InfrastructureProvider.ID)
				}
				if tc.expectedSite {
					assert.EqualValues(t, tc.instance.SiteID, got.Site.ID)
				}
				if tc.expectedInstanceType {
					assert.EqualValues(t, *tc.instance.InstanceTypeID, got.InstanceType.ID)
				}
				if tc.expectedNetworkSecurityGroup {
					assert.NotNil(t, got.NetworkSecurityGroup)
					assert.EqualValues(t, *tc.instance.NetworkSecurityGroupID, got.NetworkSecurityGroup.ID)
				}
				if tc.expectedVpc {
					assert.EqualValues(t, tc.instance.VpcID, got.Vpc.ID)
				}
				if tc.expectedMachine {
					assert.EqualValues(t, *tc.instance.MachineID, got.Machine.ID)
				}
				if tc.expectedOperatingSystem {
					assert.EqualValues(t, *tc.instance.OperatingSystemID, got.OperatingSystem.ID)
				}
				assert.Equal(t, got.AlwaysBootWithCustomIpxe, true)
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
					_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
					assert.True(t, ok)
				}
			}
		})
	}
}

func TestInstanceSQLDAO_GetCountByStatus(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
	}

	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant1 := testInstanceBuildTenant(t, dbSession, "testTenant1")
	tenant2 := testInstanceBuildTenant(t, dbSession, "testTenant2")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant1, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant1, site, "testNetworkSecurityGroup")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant1, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)

	i1, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant1.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			NetworkSecurityGroupID:   &networkSecurityGroup.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			PhoneHomeEnabled:         true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.NoError(t, err)
	assert.NotNil(t, i1)

	i2, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test2",
			TenantID:                 tenant1.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			PhoneHomeEnabled:         true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.NoError(t, err)
	assert.NotNil(t, i2)

	i3, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test3",
			TenantID:                 tenant1.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			PhoneHomeEnabled:         true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   InstanceStatusProvisioning,
			CreatedBy:                user.ID,
		},
	)
	assert.NoError(t, err)
	assert.NotNil(t, i3)

	i4, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test4",
			TenantID:                 tenant1.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   InstanceStatusReady,
			CreatedBy:                user.ID,
		},
	)
	assert.NoError(t, err)
	assert.NotNil(t, i4)

	_, err = isd.Update(ctx, nil, InstanceUpdateInput{
		InstanceID: i4.ID,
		InstanceUpdateCommonInput: InstanceUpdateCommonInput{
			PowerStatus: cutil.GetPtr(InstancePowerStatusRebooting),
		},
	})
	assert.NoError(t, err)

	i5, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test5",
			TenantID:                 tenant1.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   InstanceStatusRepairing,
			CreatedBy:                user.ID,
		},
	)
	assert.NoError(t, err)
	assert.NotNil(t, i5)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		id                 uuid.UUID
		fields             fields
		args               args
		wantErr            error
		wantEmpty          bool
		wantCount          int
		wantStatusMap      InstanceCountByStatus
		reqTenant          *uuid.UUID
		reqSite            *uuid.UUID
		verifyChildSpanner bool
	}{
		{
			name: "get instance status count by tenant with instance returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: false,
			wantCount: 5,
			wantStatusMap: InstanceCountByStatus{
				Total:        5,
				Pending:      2,
				Provisioning: 1,
				Repairing:    1,
				Rebooting:    1,
			},
			reqTenant:          cutil.GetPtr(tenant1.ID),
			verifyChildSpanner: true,
		},
		{
			name: "get instance status count by tenant with no instance returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: true,
			reqTenant: cutil.GetPtr(tenant2.ID),
		},
		{
			name: "get instance status count with no filter instance returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: false,
			wantCount: 5,
			wantStatusMap: InstanceCountByStatus{
				Total:        5,
				Pending:      2,
				Provisioning: 1,
				Repairing:    1,
				Rebooting:    1,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			isd := InstanceSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := isd.GetCountByStatus(tt.args.ctx, nil, tt.reqTenant, tt.reqSite)
			if tt.wantErr != nil {
				assert.ErrorAs(t, err, &tt.wantErr)
				return
			}
			if tt.wantEmpty {
				assert.EqualValues(t, got.Total, 0)
			}
			if err == nil && !tt.wantEmpty {
				assert.Equal(t, tt.wantStatusMap, got)
				assert.EqualValues(t, got.Pending, 2)
				assert.EqualValues(t, got.Repairing, 1)
				assert.EqualValues(t, got.Total, tt.wantCount)
			}
			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestAggregatedInstanceStatus(t *testing.T) {
	tests := []struct {
		name        string
		status      string
		powerStatus *string
		want        string
	}{
		{
			name:        "non-ready status ignores power status",
			status:      InstanceStatusPending,
			powerStatus: cutil.GetPtr(InstancePowerStatusRebooting),
			want:        InstanceStatusPending,
		},
		{
			name:        "ready with rebooting power status",
			status:      InstanceStatusReady,
			powerStatus: cutil.GetPtr(InstancePowerStatusRebooting),
			want:        InstancePowerStatusRebooting,
		},
		{
			name:        "ready with error power status",
			status:      InstanceStatusReady,
			powerStatus: cutil.GetPtr(InstancePowerStatusError),
			want:        InstancePowerStatusError,
		},
		{
			name:        "ready with boot completed power status",
			status:      InstanceStatusReady,
			powerStatus: cutil.GetPtr(InstancePowerStatusBootCompleted),
			want:        InstanceStatusReady,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			inst := &Instance{Status: tt.status, PowerStatus: tt.powerStatus}
			got := inst.GetAggregatedStatus(tt.status, tt.powerStatus)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestInstanceCountByStatus_FromQueryResults(t *testing.T) {
	tests := []struct {
		name string
		rows []instanceStatusCountQueryResult
		want InstanceCountByStatus
	}{
		{
			name: "maps known statuses and accumulates total",
			rows: []instanceStatusCountQueryResult{
				{Status: InstanceStatusPending, TotalCount: 2},
				{Status: InstanceStatusReady, TotalCount: 5},
				{Status: InstancePowerStatusRebooting, TotalCount: 1},
			},
			want: InstanceCountByStatus{
				Total:     8,
				Pending:   2,
				Ready:     5,
				Rebooting: 1,
			},
		},
		{
			name: "unknown status rolls into unknown bucket",
			rows: []instanceStatusCountQueryResult{
				{Status: "unexpected", TotalCount: 3},
			},
			want: InstanceCountByStatus{
				Total:   3,
				Unknown: 3,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var got InstanceCountByStatus
			got.FromQueryResults(tt.rows)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestInstanceSQLDAO_GetAll_AggregatedStatusFilter(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)

	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant1")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant, site, "testNetworkSecurityGroup")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)

	createInput := func(name, status string, powerStatus *string) *Instance {
		t.Helper()
		inst, err := isd.Create(ctx, nil, InstanceCreateInput{
			Name:                     name,
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			NetworkSecurityGroupID:   &networkSecurityGroup.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			Status:                   status,
			PowerStatus:              powerStatus,
			CreatedBy:                user.ID,
		})
		assert.NoError(t, err)
		return inst
	}

	rebootingInstance := createInput("rebooting-instance", InstanceStatusReady, cutil.GetPtr(InstancePowerStatusRebooting))
	readyInstance := createInput("ready-instance", InstanceStatusReady, cutil.GetPtr(InstancePowerStatusBootCompleted))
	powerErrorInstance := createInput("power-error-instance", InstanceStatusReady, cutil.GetPtr(InstancePowerStatusError))
	lifecycleErrorInstance := createInput("lifecycle-error-instance", InstanceStatusError, nil)

	rebootingMatches, rebootingTotal, err := isd.GetAll(ctx, nil, InstanceFilterInput{
		TenantIDs: []uuid.UUID{tenant.ID},
		Statuses:  []string{InstancePowerStatusRebooting},
	}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 1, rebootingTotal)
	assert.Len(t, rebootingMatches, 1)
	assert.Equal(t, rebootingInstance.ID, rebootingMatches[0].ID)

	readyMatches, readyTotal, err := isd.GetAll(ctx, nil, InstanceFilterInput{
		TenantIDs: []uuid.UUID{tenant.ID},
		Statuses:  []string{InstanceStatusReady},
	}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 1, readyTotal)
	assert.Equal(t, readyInstance.ID, readyMatches[0].ID)

	errorMatches, errorTotal, err := isd.GetAll(ctx, nil, InstanceFilterInput{
		TenantIDs: []uuid.UUID{tenant.ID},
		Statuses:  []string{InstanceStatusError},
	}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 2, errorTotal)
	errorIDs := []uuid.UUID{errorMatches[0].ID, errorMatches[1].ID}
	assert.Contains(t, errorIDs, powerErrorInstance.ID)
	assert.Contains(t, errorIDs, lifecycleErrorInstance.ID)
}

func TestInstanceSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	site2 := testInstanceBuildSite(t, dbSession, ip, "testSite2")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant1")
	tenant2 := testInstanceBuildTenant(t, dbSession, "testTenant2")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, site2, tenant2, "testVpc2")
	vpcSelection := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpcSelection")
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	allocation2 := testInstanceBuildAllocation(t, dbSession, ip, tenant2, site2, "testAllocation2")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant, site, "testNetworkSecurityGroup")
	instanceType2 := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType2")
	networkSecurityGroup2 := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant2, site2, "testNetworkSecurityGroup2")
	ipBlock := &IPBlock{
		ID:                       uuid.New(),
		Name:                     "testIpBlock",
		SiteID:                   site.ID,
		InfrastructureProviderID: ip.ID,
		TenantID:                 &tenant.ID,
		RoutingType:              IPBlockRoutingTypeDatacenterOnly,
		Prefix:                   "10.0.0.0",
		PrefixLength:             24,
		ProtocolVersion:          IPBlockProtocolVersionV4,
		Status:                   IPBlockStatusReady,
	}
	_, err := dbSession.DB.NewInsert().Model(ipBlock).Exec(ctx)
	assert.NoError(t, err)
	ipBlock2 := &IPBlock{
		ID:                       uuid.New(),
		Name:                     "testIpBlock2",
		SiteID:                   site2.ID,
		InfrastructureProviderID: ip.ID,
		TenantID:                 &tenant2.ID,
		RoutingType:              IPBlockRoutingTypeDatacenterOnly,
		Prefix:                   "10.1.0.0",
		PrefixLength:             24,
		ProtocolVersion:          IPBlockProtocolVersionV4,
		Status:                   IPBlockStatusReady,
	}
	_, err = dbSession.DB.NewInsert().Model(ipBlock2).Exec(ctx)
	assert.NoError(t, err)
	vpcPrefix := &VpcPrefix{
		ID:           uuid.New(),
		Name:         "testVpcPrefix",
		Org:          tenant.Org,
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IPBlockID:    &ipBlock.ID,
		Prefix:       "10.0.0.0/24",
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    uuid.New(),
	}
	_, err = dbSession.DB.NewInsert().Model(vpcPrefix).Exec(ctx)
	assert.NoError(t, err)
	vpcPrefix2 := &VpcPrefix{
		ID:           uuid.New(),
		Name:         "testVpcPrefix2",
		Org:          tenant2.Org,
		SiteID:       site2.ID,
		VpcID:        vpc2.ID,
		TenantID:     tenant2.ID,
		IPBlockID:    &ipBlock2.ID,
		Prefix:       "10.1.0.0/24",
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    uuid.New(),
	}
	_, err = dbSession.DB.NewInsert().Model(vpcPrefix2).Exec(ctx)
	assert.NoError(t, err)
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	testBuildAllocationConstraint(t, dbSession, allocation2, AllocationResourceTypeInstanceType, instanceType2.ID, AllocationConstraintTypeReserved, 10, uuid.New())

	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	operatingSystem2 := testInstanceBuildOperatingSystem(t, dbSession, "testOS2")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)
	dummyUUID := uuid.New()

	mcd := NewMachineCapabilityDAO(dbSession)

	totalCount := 30

	instanceGroup1 := []Instance{}
	for i := 0; i < totalCount/2; i++ {
		machine := testMachineBuildMachineWithID(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest1"), fmt.Sprintf("fm100-%d", i))

		_, err := mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &machine.ID, InstanceTypeID: &instanceType.ID, Type: MachineCapabilityTypeInfiniBand, Name: "Test Capability",
			Frequency: cutil.GetPtr("3 GHz"), Capacity: cutil.GetPtr("12 TB"), Vendor: cutil.GetPtr("Test Vendor"), Count: cutil.GetPtr(1)})
		assert.NoError(t, err)

		instance, err := isd.Create(
			ctx, nil,
			InstanceCreateInput{
				Name:                     fmt.Sprintf("test-%d", i),
				TenantID:                 tenant.ID,
				InfrastructureProviderID: ip.ID,
				SiteID:                   site.ID,
				InstanceTypeID:           &instanceType.ID,
				NetworkSecurityGroupID:   &networkSecurityGroup.ID,
				VpcID:                    vpc.ID,
				MachineID:                &machine.ID,
				Hostname:                 cutil.GetPtr("test.com"),
				OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
				IpxeScript:               cutil.GetPtr("ipxe"),
				AlwaysBootWithCustomIpxe: true,
				UserData:                 cutil.GetPtr("userdata"),
				Labels:                   map[string]string{fmt.Sprintf("region%v", i): fmt.Sprintf("west%v", i)},
				Status:                   InstanceStatusPending,
				PowerStatus:              cutil.GetPtr(InstancePowerStatusBootCompleted),
				CreatedBy:                user.ID,
			},
		)

		assert.Nil(t, err)
		assert.NotNil(t, instance)
		instanceGroup1 = append(instanceGroup1, *instance)
	}

	instanceGroup2 := []Instance{}
	for i := 0; i < totalCount/2; i++ {
		machine := testMachineBuildMachineWithID(t, dbSession, ip.ID, site2.ID, &instanceType2.ID, cutil.GetPtr("mcTypeTest1"), fmt.Sprintf("fm200-%d", i))

		instance, err := isd.Create(
			ctx, nil,
			InstanceCreateInput{
				Name:                     fmt.Sprintf("test-%d", i),
				TenantID:                 tenant2.ID,
				InfrastructureProviderID: ip.ID,
				SiteID:                   site2.ID,
				InstanceTypeID:           &instanceType2.ID,
				NetworkSecurityGroupID:   &networkSecurityGroup2.ID,
				VpcID:                    vpc2.ID,
				MachineID:                &machine.ID,
				Hostname:                 cutil.GetPtr("test.com"),
				OperatingSystemID:        cutil.GetPtr(operatingSystem2.ID),
				IpxeScript:               cutil.GetPtr("ipxe"),
				AlwaysBootWithCustomIpxe: true,
				UserData:                 cutil.GetPtr("userdata"),
				Labels:                   map[string]string{},
				Status:                   InstanceStatusPending,
				CreatedBy:                user.ID,
			},
		)
		assert.Nil(t, err)
		assert.NotNil(t, instance)
		instanceGroup2 = append(instanceGroup2, *instance)
	}

	ifcd := NewInterfaceDAO(dbSession)
	_, err = ifcd.Create(ctx, nil, InterfaceCreateInput{
		InstanceID:  instanceGroup1[0].ID,
		VpcPrefixID: &vpcPrefix2.ID,
		Status:      InterfaceStatusPending,
		IsPhysical:  true,
		CreatedBy:   user.ID,
	})
	assert.NoError(t, err)
	_, err = ifcd.Create(ctx, nil, InterfaceCreateInput{
		InstanceID:  instanceGroup1[0].ID,
		VpcPrefixID: &vpcPrefix.ID,
		Status:      InterfaceStatusPending,
		IsPhysical:  false,
		CreatedBy:   user.ID,
	})
	assert.NoError(t, err)
	_, err = ifcd.Create(ctx, nil, InterfaceCreateInput{
		InstanceID:  instanceGroup2[0].ID,
		VpcPrefixID: &vpcPrefix.ID,
		Status:      InterfaceStatusPending,
		IsPhysical:  true,
		CreatedBy:   user.ID,
	})
	assert.NoError(t, err)
	_, err = ifcd.Create(ctx, nil, InterfaceCreateInput{
		InstanceID:      instanceGroup1[1].ID,
		VpcID:           &vpcSelection.ID,
		VpcIPFamilyMode: cutil.GetPtr(InterfaceVpcIPFamilyModeIPv4Only),
		Status:          InterfaceStatusPending,
		IsPhysical:      true,
		CreatedBy:       user.ID,
	})
	assert.NoError(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                string
		filter              InstanceFilterInput
		PageInput           paginator.PageInput
		firstEntry          *Instance
		expectedCount       int
		expectedTotal       *int
		expectedError       bool
		paramRelations      []string
		expectedPowerStatus *string
		verifyChildSpanner  bool
	}{
		{
			desc:                "GetAll with no filters returns objects",
			expectedCount:       paginator.DefaultLimit,
			expectedTotal:       &totalCount,
			expectedError:       false,
			expectedPowerStatus: cutil.GetPtr(InstancePowerStatusBootCompleted),
			verifyChildSpanner:  true,
		},
		{
			desc:          "GetAll with all relations",
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
			paramRelations: []string{
				InfrastructureProviderRelationName,
				SiteRelationName,
				InstanceTypeRelationName,
				TenantRelationName,
				VpcRelationName,
				OperatingSystemRelationName,
			},
		},
		{
			desc: "GetAll with single instance ID filter returns object",
			filter: InstanceFilterInput{
				InstanceIDs: []uuid.UUID{instanceGroup1[0].ID},
			},
			expectedCount: 1,
			expectedError: false,
			firstEntry:    &instanceGroup1[0],
		},
		{
			desc: "GetAll with multiple instance IDs filter returns objects",
			filter: InstanceFilterInput{
				InstanceIDs: []uuid.UUID{instanceGroup1[0].ID, instanceGroup1[1].ID, instanceGroup2[0].ID},
			},
			expectedCount: 3,
			expectedError: false,
		},
		{
			desc: "GetAll with instance IDs filter returns no objects for non-existent ID",
			filter: InstanceFilterInput{
				InstanceIDs: []uuid.UUID{dummyUUID},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with tenant filter returns objects",
			filter: InstanceFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with multiple values in tenant filter returns objects",
			filter: InstanceFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID, tenant2.ID},
			},
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc: "GetAll with tenant and name filters returns objects",
			filter: InstanceFilterInput{
				Names:     []string{"test-0"},
				TenantIDs: []uuid.UUID{tenant.ID},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with infrastructureprovider filter returns objects",
			filter: InstanceFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
			},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc: "GetAll with site filter returns objects",
			filter: InstanceFilterInput{
				SiteIDs: []uuid.UUID{site.ID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with multiple site filter values returns objects",
			filter: InstanceFilterInput{
				SiteIDs: []uuid.UUID{site.ID, site2.ID},
			},
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc: "GetAll with instancetype filter returns objects",
			filter: InstanceFilterInput{
				InstanceTypeIDs: []uuid.UUID{instanceType.ID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with multiple values for instancetype filter returns objects",
			filter: InstanceFilterInput{
				InstanceTypeIDs: []uuid.UUID{instanceType.ID, instanceType2.ID},
			},
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc: "GetAll with vpc filter returns objects",
			filter: InstanceFilterInput{
				VpcIDs: []uuid.UUID{vpc.ID},
			},
			expectedCount: totalCount/2 + 1, // plus 1 because of the instance attached to the vpc via the secondary interface.
			expectedError: false,
		},
		{
			desc: "GetAll with multiple values for vpc filter returns objects",
			filter: InstanceFilterInput{
				VpcIDs: []uuid.UUID{vpc.ID, vpc2.ID},
			},
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc: "GetAll with vpc filter returns instances attached through secondary VPC interfaces",
			filter: InstanceFilterInput{
				VpcIDs: []uuid.UUID{vpc2.ID},
			},
			PageInput: paginator.PageInput{
				Limit: cutil.GetPtr(totalCount),
			},
			expectedCount: totalCount/2 + 1,
			expectedError: false,
		},
		{
			desc: "VPC filter includes an unresolved directly selected secondary VPC",
			filter: InstanceFilterInput{
				VpcIDs: []uuid.UUID{vpcSelection.ID},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with machine filter returns objects",
			filter: InstanceFilterInput{
				MachineIDs: []string{*instanceGroup1[0].MachineID, *instanceGroup1[1].MachineID, *instanceGroup1[2].MachineID},
			},
			expectedCount: 3,
			expectedError: false,
		},
		{
			desc: "GetAll with operatingsystem filter returns objects",
			filter: InstanceFilterInput{
				OperatingSystemIDs: []uuid.UUID{operatingSystem.ID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with multiple values for operatingsystem filter returns objects",
			filter: InstanceFilterInput{
				OperatingSystemIDs: []uuid.UUID{operatingSystem.ID, operatingSystem2.ID},
			},
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc: "GetAll with all filters returns objects",
			filter: InstanceFilterInput{
				TenantIDs:                 []uuid.UUID{tenant.ID},
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
				InstanceTypeIDs:           []uuid.UUID{instanceType.ID},
				NetworkSecurityGroupIDs:   []string{networkSecurityGroup.ID},
				VpcIDs:                    []uuid.UUID{vpc.ID},
				MachineIDs:                []string{*instanceGroup1[0].MachineID},
				OperatingSystemIDs:        []uuid.UUID{operatingSystem.ID},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with all filters returns no objects",
			filter: InstanceFilterInput{
				TenantIDs:                 []uuid.UUID{tenant2.ID},
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
				InstanceTypeIDs:           []uuid.UUID{instanceType.ID},
				NetworkSecurityGroupIDs:   []string{networkSecurityGroup.ID},
				VpcIDs:                    []uuid.UUID{vpc2.ID},
				MachineIDs:                []string{*instanceGroup2[0].MachineID},
				OperatingSystemIDs:        []uuid.UUID{operatingSystem.ID},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with some filters returns objects",
			filter: InstanceFilterInput{
				TenantIDs:                 []uuid.UUID{tenant.ID},
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with some filters returns no objects",
			filter: InstanceFilterInput{
				TenantIDs:                 []uuid.UUID{tenant2.ID},
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with limit returns objects",
			filter: InstanceFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
			},
			PageInput: paginator.PageInput{
				Offset: cutil.GetPtr(0),
				Limit:  cutil.GetPtr(5),
			},
			expectedCount: 5,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with offset returns objects",
			filter: InstanceFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
			},
			PageInput: paginator.PageInput{
				Offset: cutil.GetPtr(5),
			},
			expectedCount: 10,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with order by returns objects",
			filter: InstanceFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
			},
			PageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "name",
					Order: paginator.OrderAscending,
				},
			},
			firstEntry:    &instanceGroup1[0],
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with name search query returns objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr("test-"),
			},
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc: "GetAll with status search query returns objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr(InstanceStatusPending),
			},
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc: "GetAll with label query returns no objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr("region250"),
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with label query returns multiple objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr("region1"),
			},
			expectedCount: 6,
			expectedError: false,
		},
		{
			desc: "GetAll with status search query returns no objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr(InstanceStatusReady),
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with combination of name and status search query returns objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr("test- ready"),
			},
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc: "GetAll with empty search query returns objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr(""),
			},
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc: "GetAll with InstanceStatusPending status returns objects",
			filter: InstanceFilterInput{
				Statuses: []string{InstanceStatusPending},
			},
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc: "GetAll with InstanceStatusError status returns no objects",
			filter: InstanceFilterInput{
				Statuses: []string{InstanceStatusError},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with status of InstanceStatusError or InstanceStatusPending returns objects",
			filter: InstanceFilterInput{
				Statuses: []string{InstanceStatusError, InstanceStatusPending},
			},
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc: "GetAll with order by machine id",
			filter: InstanceFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
			},
			PageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: instanceOrderByMachineID,
					Order: paginator.OrderAscending,
				},
			},
			firstEntry:    &instanceGroup1[0],
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with order by tenant name no tenant relation",
			filter: InstanceFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
			},
			PageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: instanceOrderByTenantOrgDisplayNameExt,
					Order: paginator.OrderAscending,
				},
			},
			firstEntry:    &instanceGroup1[0],
			expectedCount: 20,
			expectedTotal: cutil.GetPtr(totalCount),
			expectedError: false,
		},
		{
			desc: "GetAll with order by tenant name with tenant relation",
			filter: InstanceFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
			},
			PageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: instanceOrderByTenantOrgDisplayNameExt,
					Order: paginator.OrderAscending,
				},
			},
			firstEntry:     &instanceGroup1[0],
			expectedCount:  20,
			expectedTotal:  cutil.GetPtr(totalCount),
			expectedError:  false,
			paramRelations: []string{TenantRelationName},
		},
		{
			desc: "GetAll with order by instance type name no instance type relation",
			filter: InstanceFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
			},
			PageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: instanceOrderByInstanceTypeNameExt,
					Order: paginator.OrderAscending,
				},
			},
			firstEntry:    &instanceGroup1[0],
			expectedCount: 20,
			expectedTotal: cutil.GetPtr(totalCount),
			expectedError: false,
		},
		{
			desc: "GetAll with order by instance type name with instance type relation",
			filter: InstanceFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
			},
			PageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: instanceOrderByInstanceTypeNameExt,
					Order: paginator.OrderAscending,
				},
			},
			firstEntry:     &instanceGroup1[0],
			expectedCount:  20,
			expectedTotal:  cutil.GetPtr(totalCount),
			expectedError:  false,
			paramRelations: []string{InstanceTypeRelationName},
		},
		{
			desc: "GetAll with order by has infinibad",
			filter: InstanceFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
			},
			PageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: instanceOrderByHasInfiniBandExt,
					Order: paginator.OrderAscending,
				},
			},
			firstEntry:    &instanceGroup1[0],
			expectedCount: 20,
			expectedTotal: cutil.GetPtr(totalCount),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := isd.GetAll(ctx, nil,
				tc.filter,
				tc.PageInput,
				tc.paramRelations,
			)
			if err != nil && !tc.expectedError {
				fmt.Printf("\n%s\n", err)
			}
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
				assert.Equal(t, tc.expectedCount, len(got))
				for _, relation := range tc.paramRelations {
					if relation == TenantRelationName {
						assert.NotNil(t, got[0].Tenant)
					} else if relation == InfrastructureProviderRelationName {
						assert.NotNil(t, got[0].InfrastructureProvider)
					} else if relation == SiteRelationName {
						assert.NotNil(t, got[0].Site)
					} else if relation == InstanceTypeRelationName {
						assert.NotNil(t, got[0].InstanceType)
					} else if relation == VpcRelationName {
						assert.NotNil(t, got[0].Vpc)
					} else if relation == OperatingSystemRelationName {
						assert.NotNil(t, got[0].OperatingSystem)
					}
				}
				if tc.expectedPowerStatus != nil {
					assert.Equal(t, *got[0].PowerStatus, InstancePowerStatusBootCompleted)
				}
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.firstEntry != nil {
				assert.Equal(t, tc.firstEntry.ID, got[0].ID, fmt.Sprintf("%v - %v", tc.firstEntry.ID, got[0].ID))
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

// TODO: Remove this once the migration to drop allocation_id and allocation_constraint_id columns is complete.
type InstanceWithAllocation struct {
	bun.BaseModel `bun:"table:instance,alias:i"`

	ID                                     uuid.UUID                               `bun:"type:uuid,pk"`
	Name                                   string                                  `bun:"name,notnull"`
	Description                            *string                                 `bun:"description"`
	AllocationID                           *uuid.UUID                              `bun:"allocation_id,type:uuid"`
	Allocation                             *Allocation                             `bun:"rel:belongs-to,join:allocation_id=id"`
	AllocationConstraintID                 *uuid.UUID                              `bun:"allocation_constraint_id,type:uuid"`
	AllocationConstraint                   *AllocationConstraint                   `bun:"rel:belongs-to,join:allocation_constraint_id=id"`
	TenantID                               uuid.UUID                               `bun:"tenant_id,type:uuid,notnull"`
	Tenant                                 *Tenant                                 `bun:"rel:belongs-to,join:tenant_id=id"`
	InfrastructureProviderID               uuid.UUID                               `bun:"infrastructure_provider_id,type:uuid,notnull"`
	InfrastructureProvider                 *InfrastructureProvider                 `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
	SiteID                                 uuid.UUID                               `bun:"site_id,type:uuid,notnull"`
	Site                                   *Site                                   `bun:"rel:belongs-to,join:site_id=id"`
	NetworkSecurityGroupID                 *string                                 `bun:"network_security_group_id"`
	NetworkSecurityGroup                   *NetworkSecurityGroup                   `bun:"rel:belongs-to,join:network_security_group_id=id"`
	NetworkSecurityGroupPropagationDetails *NetworkSecurityGroupPropagationDetails `bun:"network_security_group_propagation_details,type:jsonb"`
	InstanceTypeID                         *uuid.UUID                              `bun:"instance_type_id,type:uuid"`
	InstanceType                           *InstanceType                           `bun:"rel:belongs-to,join:instance_type_id=id"`
	VpcID                                  uuid.UUID                               `bun:"vpc_id,type:uuid,notnull"`
	Vpc                                    *Vpc                                    `bun:"rel:belongs-to,join:vpc_id=id"`
	MachineID                              *string                                 `bun:"machine_id"`
	Machine                                *Machine                                `bun:"rel:belongs-to,join:machine_id=id"`
	ControllerInstanceID                   *uuid.UUID                              `bun:"controller_instance_id,type:uuid"`
	Hostname                               *string                                 `bun:"hostname"`
	OperatingSystemID                      *uuid.UUID                              `bun:"operating_system_id,type:uuid"`
	OperatingSystem                        *OperatingSystem                        `bun:"rel:belongs-to,join:operating_system_id=id"`
	IpxeScript                             *string                                 `bun:"ipxe_script"`
	AlwaysBootWithCustomIpxe               bool                                    `bun:"always_boot_with_custom_ipxe,notnull"`
	PhoneHomeEnabled                       bool                                    `bun:"phone_home_enabled,notnull"`
	UserData                               *string                                 `bun:"user_data"`
	AutoNetwork                            bool                                    `bun:"auto_network,notnull"`
	Labels                                 map[string]string                       `bun:"labels,type:jsonb"`
	IsUpdatePending                        bool                                    `bun:"is_update_pending,notnull"`
	InfinityRCRStatus                      *string                                 `bun:"infinity_rcr_status"`
	TpmEkCertificate                       *string                                 `bun:"tpm_ek_certificate"`
	Status                                 string                                  `bun:"status,notnull"`
	PowerStatus                            *string                                 `bun:"power_status"`
	PowerProfile                           *string                                 `bun:"power_profile"`
	IsMissingOnSite                        bool                                    `bun:"is_missing_on_site,notnull"`
	Created                                time.Time                               `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated                                time.Time                               `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted                                *time.Time                              `bun:"deleted,soft_delete"`
	CreatedBy                              uuid.UUID                               `bun:"created_by,type:uuid,notnull"`
	// Not for display, used by the query that sorts on machine capability type, specifically InfiniBand type
	MCType string `bun:"mc_type,scanonly"`
}

func (iwa *InstanceWithAllocation) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("allocation_id") REFERENCES "allocation" ("id")`).
		ForeignKey(`("allocation_constraint_id") REFERENCES "allocation_constraint" ("id")`).
		ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`).
		ForeignKey(`("infrastructure_provider_id") REFERENCES "infrastructure_provider" ("id")`).
		ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("instance_type_id") REFERENCES "instance_type" ("id")`).
		ForeignKey(`("vpc_id") REFERENCES "vpc" ("id")`).
		ForeignKey(`("machine_id") REFERENCES "machine" ("id")`).
		ForeignKey(`("operating_system_id") REFERENCES "operating_system" ("id")`).
		ForeignKey(`("network_security_group_id") REFERENCES "network_security_group" ("id")`)
	return nil
}

// TODO: Remove this once the migration to drop allocation_id and allocation_constraint_id columns is complete.
func TestInstanceSQLDAO_GetAll_WithUnknownColumns(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
	}

	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	// create Allocation table
	err := dbSession.DB.ResetModel(context.Background(), (*Allocation)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*InstanceType)(nil))
	assert.Nil(t, err)
	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*Vpc)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*IPBlock)(nil))
	assert.Nil(t, err)
	// create VpcPrefix table
	err = dbSession.DB.ResetModel(context.Background(), (*VpcPrefix)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*Machine)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*MachineCapability)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*OperatingSystem)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*InstanceWithAllocation)(nil))
	assert.Nil(t, err)
	// create Interface table
	err = dbSession.DB.ResetModel(context.Background(), (*Interface)(nil))
	assert.Nil(t, err)
	// create SSHKey table
	err = dbSession.DB.ResetModel(context.Background(), (*SSHKey)(nil))
	assert.Nil(t, err)
	// create SSHKeyGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*SSHKeyGroup)(nil))
	assert.Nil(t, err)

	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant1 := testInstanceBuildTenant(t, dbSession, "testTenant1")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant1, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant1, site, "testNetworkSecurityGroup")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant1, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)

	i1, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant1.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			NetworkSecurityGroupID:   &networkSecurityGroup.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			PhoneHomeEnabled:         true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.NoError(t, err)
	assert.NotNil(t, i1)

	i2, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test2",
			TenantID:                 tenant1.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			PhoneHomeEnabled:         true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.NoError(t, err)
	assert.NotNil(t, i2)

	_, _, err = isd.GetAll(ctx, nil, InstanceFilterInput{}, paginator.PageInput{}, []string{})
	assert.NoError(t, err)
}

func TestInstanceSQLDAO_GetCount(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	site2 := testInstanceBuildSite(t, dbSession, ip, "testSite2")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	tenant2 := testInstanceBuildTenant(t, dbSession, "testTenant2")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, site2, tenant2, "testVpc2")
	vpcSelection := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpcSelection")
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	allocation2 := testInstanceBuildAllocation(t, dbSession, ip, tenant2, site2, "testAllocation2")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	instanceType2 := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType2")
	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant, site, "testNetworkSecurityGroup")
	networkSecurityGroup2 := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant2, site2, "testNetworkSecurityGroup2")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	_ = testBuildAllocationConstraint(t, dbSession, allocation2, AllocationResourceTypeInstanceType, instanceType2.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	ipBlock := &IPBlock{
		ID:                       uuid.New(),
		Name:                     "testIpBlock",
		SiteID:                   site.ID,
		InfrastructureProviderID: ip.ID,
		TenantID:                 &tenant.ID,
		RoutingType:              IPBlockRoutingTypeDatacenterOnly,
		Prefix:                   "10.2.0.0",
		PrefixLength:             24,
		ProtocolVersion:          IPBlockProtocolVersionV4,
		Status:                   IPBlockStatusReady,
	}
	_, err := dbSession.DB.NewInsert().Model(ipBlock).Exec(ctx)
	assert.NoError(t, err)
	ipBlock2 := &IPBlock{
		ID:                       uuid.New(),
		Name:                     "testIpBlock2",
		SiteID:                   site2.ID,
		InfrastructureProviderID: ip.ID,
		TenantID:                 &tenant2.ID,
		RoutingType:              IPBlockRoutingTypeDatacenterOnly,
		Prefix:                   "10.3.0.0",
		PrefixLength:             24,
		ProtocolVersion:          IPBlockProtocolVersionV4,
		Status:                   IPBlockStatusReady,
	}
	_, err = dbSession.DB.NewInsert().Model(ipBlock2).Exec(ctx)
	assert.NoError(t, err)
	vpcPrefix := &VpcPrefix{
		ID:           uuid.New(),
		Name:         "testVpcPrefix",
		Org:          tenant.Org,
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IPBlockID:    &ipBlock.ID,
		Prefix:       "10.2.0.0/24",
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    uuid.New(),
	}
	_, err = dbSession.DB.NewInsert().Model(vpcPrefix).Exec(ctx)
	assert.NoError(t, err)
	vpcPrefix2 := &VpcPrefix{
		ID:           uuid.New(),
		Name:         "testVpcPrefix2",
		Org:          tenant2.Org,
		SiteID:       site2.ID,
		VpcID:        vpc2.ID,
		TenantID:     tenant2.ID,
		IPBlockID:    &ipBlock2.ID,
		Prefix:       "10.3.0.0/24",
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    uuid.New(),
	}
	_, err = dbSession.DB.NewInsert().Model(vpcPrefix2).Exec(ctx)
	assert.NoError(t, err)

	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	operatingSystem2 := testInstanceBuildOperatingSystem(t, dbSession, "testOS2")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)

	totalCount := 30

	instanceGroup1 := []Instance{}
	for i := 0; i < totalCount/2; i++ {
		machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest1"))

		instance, err := isd.Create(
			ctx, nil,
			InstanceCreateInput{
				Name:                     fmt.Sprintf("test-%d", i),
				TenantID:                 tenant.ID,
				InfrastructureProviderID: ip.ID,
				SiteID:                   site.ID,
				InstanceTypeID:           &instanceType.ID,
				NetworkSecurityGroupID:   &networkSecurityGroup.ID,
				VpcID:                    vpc.ID,
				MachineID:                &machine.ID,
				Hostname:                 cutil.GetPtr("test.com"),
				OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
				IpxeScript:               cutil.GetPtr("ipxe"),
				AlwaysBootWithCustomIpxe: true,
				UserData:                 cutil.GetPtr("userdata"),
				Labels:                   map[string]string{fmt.Sprintf("region%v", i): fmt.Sprintf("west%v", i)},
				Status:                   InstanceStatusPending,
				PowerStatus:              cutil.GetPtr(InstancePowerStatusBootCompleted),
				CreatedBy:                user.ID,
			},
		)

		assert.Nil(t, err)
		assert.NotNil(t, instance)
		instanceGroup1 = append(instanceGroup1, *instance)
	}

	instanceGroup2 := []Instance{}
	for i := 0; i < totalCount/2; i++ {
		machine := testMachineBuildMachine(t, dbSession, ip.ID, site2.ID, &instanceType2.ID, cutil.GetPtr("mcTypeTest1"))

		instance, err := isd.Create(
			ctx, nil,
			InstanceCreateInput{
				Name:                     fmt.Sprintf("test-%d", i),
				TenantID:                 tenant2.ID,
				InfrastructureProviderID: ip.ID,
				SiteID:                   site2.ID,
				InstanceTypeID:           &instanceType2.ID,
				NetworkSecurityGroupID:   &networkSecurityGroup2.ID,
				VpcID:                    vpc2.ID,
				MachineID:                &machine.ID,
				Hostname:                 cutil.GetPtr("test.com"),
				OperatingSystemID:        cutil.GetPtr(operatingSystem2.ID),
				IpxeScript:               cutil.GetPtr("ipxe"),
				AlwaysBootWithCustomIpxe: true,
				UserData:                 cutil.GetPtr("userdata"),
				Labels:                   map[string]string{},
				Status:                   InstanceStatusPending,
				CreatedBy:                user.ID,
			},
		)
		assert.Nil(t, err)
		assert.NotNil(t, instance)
		instanceGroup2 = append(instanceGroup2, *instance)
	}

	ifcd := NewInterfaceDAO(dbSession)
	_, err = ifcd.Create(ctx, nil, InterfaceCreateInput{
		InstanceID:  instanceGroup1[0].ID,
		VpcPrefixID: &vpcPrefix2.ID,
		Status:      InterfaceStatusPending,
		IsPhysical:  true,
		CreatedBy:   user.ID,
	})
	assert.NoError(t, err)
	_, err = ifcd.Create(ctx, nil, InterfaceCreateInput{
		InstanceID:  instanceGroup1[0].ID,
		VpcPrefixID: &vpcPrefix.ID,
		Status:      InterfaceStatusPending,
		IsPhysical:  false,
		CreatedBy:   user.ID,
	})
	assert.NoError(t, err)
	_, err = ifcd.Create(ctx, nil, InterfaceCreateInput{
		InstanceID:  instanceGroup2[0].ID,
		VpcPrefixID: &vpcPrefix.ID,
		Status:      InterfaceStatusPending,
		IsPhysical:  true,
		CreatedBy:   user.ID,
	})
	assert.NoError(t, err)
	_, err = ifcd.Create(ctx, nil, InterfaceCreateInput{
		InstanceID:      instanceGroup1[1].ID,
		VpcID:           &vpcSelection.ID,
		VpcIPFamilyMode: cutil.GetPtr(InterfaceVpcIPFamilyModeIPv4Only),
		Status:          InterfaceStatusPending,
		IsPhysical:      true,
		CreatedBy:       user.ID,
	})
	assert.NoError(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		filter             InstanceFilterInput
		expectedCount      int
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "GetCount with no filters returns objects",
			expectedCount:      totalCount,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "GetCount with tenant filter returns objects",
			filter: InstanceFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetCount with multiple values in tenant filter returns objects",
			filter: InstanceFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID, tenant2.ID},
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with tenant and name filters returns objects",
			filter: InstanceFilterInput{
				Names:     []string{"test-0"},
				TenantIDs: []uuid.UUID{tenant.ID},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetCount with infrastructureprovider filter returns objects",
			filter: InstanceFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with site filter returns objects",
			filter: InstanceFilterInput{
				SiteIDs: []uuid.UUID{site.ID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetCount with multiple site filter values returns objects",
			filter: InstanceFilterInput{
				SiteIDs: []uuid.UUID{site.ID, site2.ID},
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with instancetype filter returns objects",
			filter: InstanceFilterInput{
				InstanceTypeIDs: []uuid.UUID{instanceType.ID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetCount with multiple values for instancetype filter returns objects",
			filter: InstanceFilterInput{
				InstanceTypeIDs: []uuid.UUID{instanceType.ID, instanceType2.ID},
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with vpc filter returns objects",
			filter: InstanceFilterInput{
				VpcIDs: []uuid.UUID{vpc.ID},
			},
			expectedCount: totalCount/2 + 1, // plus 1 because of the instance attached to the vpc via the secondary interface.
			expectedError: false,
		},
		{
			desc: "GetCount with multiple values for vpc filter returns objects",
			filter: InstanceFilterInput{
				VpcIDs: []uuid.UUID{vpc.ID, vpc2.ID},
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with vpc filter includes instances attached through secondary VPC interfaces",
			filter: InstanceFilterInput{
				VpcIDs: []uuid.UUID{vpc2.ID},
			},
			expectedCount: totalCount/2 + 1,
			expectedError: false,
		},
		{
			desc: "VPC filter count includes an unresolved directly selected secondary VPC",
			filter: InstanceFilterInput{
				VpcIDs: []uuid.UUID{vpcSelection.ID},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetCount with machine filter returns objects",
			filter: InstanceFilterInput{
				MachineIDs: []string{*instanceGroup1[0].MachineID, *instanceGroup1[1].MachineID, *instanceGroup1[2].MachineID},
			},
			expectedCount: 3,
			expectedError: false,
		},
		{
			desc: "GetCount with operatingsystem filter returns objects",
			filter: InstanceFilterInput{
				OperatingSystemIDs: []uuid.UUID{operatingSystem.ID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetCount with multiple values for operatingsystem filter returns objects",
			filter: InstanceFilterInput{
				OperatingSystemIDs: []uuid.UUID{operatingSystem.ID, operatingSystem2.ID},
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with all filters returns objects",
			filter: InstanceFilterInput{
				TenantIDs:                 []uuid.UUID{tenant.ID},
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
				InstanceTypeIDs:           []uuid.UUID{instanceType.ID},
				VpcIDs:                    []uuid.UUID{vpc.ID},
				MachineIDs:                []string{*instanceGroup1[0].MachineID},
				OperatingSystemIDs:        []uuid.UUID{operatingSystem.ID},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetCount with all filters returns no objects",
			filter: InstanceFilterInput{
				TenantIDs:                 []uuid.UUID{tenant2.ID},
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
				InstanceTypeIDs:           []uuid.UUID{instanceType.ID},
				VpcIDs:                    []uuid.UUID{vpc2.ID},
				MachineIDs:                []string{*instanceGroup1[0].MachineID},
				OperatingSystemIDs:        []uuid.UUID{operatingSystem.ID},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetCount with some filters returns objects",
			filter: InstanceFilterInput{
				TenantIDs:                 []uuid.UUID{tenant.ID},
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetCount with some filters returns no objects",
			filter: InstanceFilterInput{
				TenantIDs:                 []uuid.UUID{tenant2.ID},
				InfrastructureProviderIDs: []uuid.UUID{ip.ID},
				SiteIDs:                   []uuid.UUID{site.ID},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetCount with name search query returns objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr("test-"),
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with status search query returns objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr(InstanceStatusPending),
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with label query returns no objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr("region250"),
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetCount with label query returns multiple objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr("region1"),
			},
			expectedCount: 6,
			expectedError: false,
		},
		{
			desc: "GetCount with status search query returns no objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr(InstanceStatusReady),
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetCount with combination of name and status search query returns objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr("test- ready"),
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with empty search query returns objects",
			filter: InstanceFilterInput{
				SearchQuery: cutil.GetPtr(""),
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with InstanceStatusPending status returns objects",
			filter: InstanceFilterInput{
				Statuses: []string{InstanceStatusPending},
			},
			expectedCount: totalCount,
			expectedError: false,
		},
		{
			desc: "GetCount with InstanceStatusError status returns no objects",
			filter: InstanceFilterInput{
				Statuses: []string{InstanceStatusError},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetCount with status of InstanceStatusError or InstanceStatusPending returns objects",
			filter: InstanceFilterInput{
				Statuses: []string{InstanceStatusError, InstanceStatusPending},
			},
			expectedCount: totalCount,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			count, err := isd.GetCount(ctx, nil, tc.filter)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, count)
			} else {
				assert.Equal(t, tc.expectedCount, count)
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestInstanceSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	ip2 := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP2")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	site2 := testInstanceBuildSite(t, dbSession, ip, "testSite2")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	tenant2 := testInstanceBuildTenant(t, dbSession, "testTenant2")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, site2, tenant2, "testVpc2")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	instanceType2 := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType2")
	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant, site, "testNetworkSecurityGroup")
	networkSecurityGroup2 := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant2, site2, "testNetworkSecurityGroup2")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	machine2 := testMachineBuildMachine(t, dbSession, ip2.ID, site2.ID, &instanceType2.ID, cutil.GetPtr("mcTypeTest2"))

	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	allocation2 := testInstanceBuildAllocation(t, dbSession, ip, tenant2, site2, "testAllocation2")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	_ = testBuildAllocationConstraint(t, dbSession, allocation2, AllocationResourceTypeInstanceType, instanceType2.ID, AllocationConstraintTypeReserved, 10, uuid.New())

	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	operatingSystem2 := testInstanceBuildOperatingSystem(t, dbSession, "testOS2")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)

	dummyUUID := uuid.New()
	dummyMachineID := uuid.NewString()

	i1, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			NetworkSecurityGroupID:   &networkSecurityGroup.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   InstanceStatusPending,
			PowerStatus:              cutil.GetPtr(InstancePowerStatusBootCompleted),
			CreatedBy:                user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                                        string
		id                                          uuid.UUID
		instance                                    *Instance
		paramName                                   *string
		paramDescription                            *string
		paramTenantID                               *uuid.UUID
		paramInfrastructureProviderID               *uuid.UUID
		paramSiteID                                 *uuid.UUID
		paramInstanceTypeID                         *uuid.UUID
		paramNetworkSecurityGroupID                 *string
		paramNetworkSecurityGroupPropagationDetails *NetworkSecurityGroupPropagationDetails
		paramVpcID                                  *uuid.UUID
		paramMachineID                              *string
		paramControlledInstanceID                   *uuid.UUID
		paramHostname                               *string
		paramOperatingSystemID                      *uuid.UUID
		paramIpxeScript                             *string
		paramAlwaysBootWithCustomIpxe               *bool
		paramEnablePhoneHome                        *bool
		paramUserData                               *string
		paramIsUpdatePending                        *bool
		paramInfinityRCRStatus                      *string
		paramStatus                                 *string
		paramPowerStatus                            *string
		paramIsMissingOnSite                        *bool
		paramLabels                                 map[string]string
		paramTpmEkCertificate                       *string

		expectedError                                  bool
		expectedName                                   *string
		expectedDescription                            *string
		expectedTenantID                               *uuid.UUID
		expectedInfrastructureProviderID               *uuid.UUID
		expectedSiteID                                 *uuid.UUID
		expectedInstanceTypeID                         *uuid.UUID
		expectedNetworkSecurityGroupID                 *string
		expectedNetworkSecurityGroupPropagationDetails *NetworkSecurityGroupPropagationDetails
		expectedVpcID                                  *uuid.UUID
		expectedMachineID                              *string
		expectedControllerInstanceID                   *uuid.UUID
		expectedHostname                               *string
		expectedOperatingSystemID                      *uuid.UUID
		expectedIpxeScript                             *string
		expectAlwaysBootWithCustomIpxe                 *bool
		expectEnablePhoneHome                          *bool
		expectedUserData                               *string
		expectIsUpdatePending                          *bool
		expectedInfinityRCRStatus                      *string
		expectedStatus                                 *string
		expectedPowerStatus                            *string
		expectedIsMissingOnSite                        *bool
		expectedLabels                                 map[string]string
		expectedTpmEkCertificate                       *string
		verifyChildSpanner                             bool
	}{
		{
			desc:                          "Can update string fields",
			id:                            i1.ID,
			instance:                      i1,
			paramName:                     cutil.GetPtr("updatedName"),
			paramDescription:              cutil.GetPtr("Updated description"),
			paramTenantID:                 nil,
			paramInfrastructureProviderID: nil,
			paramSiteID:                   nil,
			paramInstanceTypeID:           nil,
			paramVpcID:                    nil,
			paramMachineID:                nil,
			paramNetworkSecurityGroupID:   &networkSecurityGroup2.ID,
			paramNetworkSecurityGroupPropagationDetails: nil,

			paramHostname:          cutil.GetPtr("updated.com"),
			paramOperatingSystemID: nil,
			paramIpxeScript:        cutil.GetPtr("updatedIpxe"),
			paramUserData:          cutil.GetPtr("updatedUserData"),
			paramInfinityRCRStatus: cutil.GetPtr("RESOURCE_GRANTED"),
			paramStatus:            cutil.GetPtr(InstanceStatusReady),
			paramPowerStatus:       cutil.GetPtr(InstancePowerStatusRebooting),
			paramLabels: map[string]string{
				"region": "us-west",
				"env":    "test",
			},
			expectedError:                                  false,
			expectedName:                                   cutil.GetPtr("updatedName"),
			expectedDescription:                            cutil.GetPtr("Updated description"),
			expectedTenantID:                               &tenant.ID,
			expectedInfrastructureProviderID:               &ip.ID,
			expectedSiteID:                                 &site.ID,
			expectedInstanceTypeID:                         &instanceType.ID,
			expectedNetworkSecurityGroupID:                 &networkSecurityGroup2.ID,
			expectedNetworkSecurityGroupPropagationDetails: nil,
			expectedVpcID:                                  &vpc.ID,
			expectedMachineID:                              &machine.ID,
			expectedHostname:                               cutil.GetPtr("updated.com"),
			expectedOperatingSystemID:                      cutil.GetPtr(operatingSystem.ID),
			expectedIpxeScript:                             cutil.GetPtr("updatedIpxe"),
			expectedUserData:                               cutil.GetPtr("updatedUserData"),
			expectedInfinityRCRStatus:                      cutil.GetPtr("RESOURCE_GRANTED"),
			expectedStatus:                                 cutil.GetPtr(InstanceStatusReady),
			expectedPowerStatus:                            cutil.GetPtr(InstancePowerStatusRebooting),
			expectedLabels: map[string]string{
				"region": "us-west",
				"env":    "test",
			},
			verifyChildSpanner: true,
		},
		{
			desc:                          "Can update non-string fields",
			id:                            i1.ID,
			instance:                      i1,
			paramName:                     nil,
			paramTenantID:                 &tenant2.ID,
			paramInfrastructureProviderID: &ip2.ID,
			paramSiteID:                   &site2.ID,
			paramInstanceTypeID:           &instanceType2.ID,
			paramVpcID:                    &vpc2.ID,
			paramMachineID:                &machine2.ID,
			paramControlledInstanceID:     &dummyUUID,
			paramHostname:                 nil,
			paramOperatingSystemID:        &operatingSystem2.ID,
			paramNetworkSecurityGroupPropagationDetails: &NetworkSecurityGroupPropagationDetails{NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{}},
			paramAlwaysBootWithCustomIpxe:               cutil.GetPtr(true),
			paramEnablePhoneHome:                        cutil.GetPtr(true),
			paramIsUpdatePending:                        cutil.GetPtr(true),
			paramIpxeScript:                             nil,
			paramUserData:                               nil,
			paramStatus:                                 nil,
			paramPowerStatus:                            nil,
			paramIsMissingOnSite:                        cutil.GetPtr(true),
			paramLabels:                                 map[string]string{},

			expectedError:                    false,
			expectedName:                     cutil.GetPtr("updatedName"),
			expectedTenantID:                 &tenant2.ID,
			expectedInfrastructureProviderID: &ip2.ID,
			expectedSiteID:                   &site2.ID,
			expectedInstanceTypeID:           &instanceType2.ID,
			expectedNetworkSecurityGroupID:   &networkSecurityGroup2.ID,

			expectedVpcID:                                  &vpc2.ID,
			expectedMachineID:                              &machine2.ID,
			expectedControllerInstanceID:                   &dummyUUID,
			expectedHostname:                               cutil.GetPtr("updated.com"),
			expectedOperatingSystemID:                      &operatingSystem2.ID,
			expectedNetworkSecurityGroupPropagationDetails: &NetworkSecurityGroupPropagationDetails{NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{}},
			expectedIpxeScript:                             cutil.GetPtr("updatedIpxe"),
			expectAlwaysBootWithCustomIpxe:                 cutil.GetPtr(true),
			expectEnablePhoneHome:                          cutil.GetPtr(true),
			expectIsUpdatePending:                          cutil.GetPtr(true),
			expectedUserData:                               cutil.GetPtr("updatedUserData"),
			expectedStatus:                                 cutil.GetPtr(InstanceStatusReady),
			expectedPowerStatus:                            cutil.GetPtr(InstancePowerStatusRebooting),
			expectedIsMissingOnSite:                        cutil.GetPtr(true),
		},
		{
			desc:                          "Error on update of unknown object",
			id:                            dummyUUID,
			instance:                      i1,
			paramName:                     nil,
			paramTenantID:                 &dummyUUID,
			paramInfrastructureProviderID: &dummyUUID,
			paramSiteID:                   &dummyUUID,
			paramInstanceTypeID:           &dummyUUID,
			paramNetworkSecurityGroupID:   cutil.GetPtr(dummyUUID.String()),
			paramVpcID:                    &dummyUUID,
			paramMachineID:                &dummyMachineID,
			paramHostname:                 nil,
			paramOperatingSystemID:        &dummyUUID,
			paramIpxeScript:               nil,
			paramUserData:                 nil,
			paramStatus:                   nil,
			paramLabels:                   map[string]string{},

			expectedError: true,
		},
		{
			desc:                          "Error on update due to foreign key violation",
			id:                            i1.ID,
			instance:                      i1,
			paramName:                     nil,
			paramTenantID:                 &dummyUUID,
			paramInfrastructureProviderID: &dummyUUID,
			paramSiteID:                   &dummyUUID,
			paramInstanceTypeID:           &dummyUUID,
			paramNetworkSecurityGroupID:   cutil.GetPtr(dummyUUID.String()),
			paramVpcID:                    &dummyUUID,
			paramMachineID:                &dummyMachineID,
			paramHostname:                 nil,
			paramOperatingSystemID:        &dummyUUID,
			paramIpxeScript:               nil,
			paramUserData:                 nil,
			paramStatus:                   nil,
			paramLabels:                   map[string]string{},

			expectedError: true,
		},
		{
			desc:                          "OK when nothing is updated !",
			id:                            i1.ID,
			instance:                      i1,
			paramName:                     nil,
			paramDescription:              nil,
			paramTenantID:                 nil,
			paramInfrastructureProviderID: nil,
			paramSiteID:                   nil,
			paramInstanceTypeID:           nil,
			paramNetworkSecurityGroupID:   nil,
			paramVpcID:                    nil,
			paramMachineID:                nil,
			paramHostname:                 nil,
			paramOperatingSystemID:        nil,
			paramIpxeScript:               nil,
			paramEnablePhoneHome:          nil,
			paramUserData:                 nil,
			paramInfinityRCRStatus:        nil,
			paramStatus:                   nil,
			paramPowerStatus:              nil,
			paramIsMissingOnSite:          nil,
			paramLabels:                   map[string]string{},

			expectedError:                    false,
			expectedName:                     cutil.GetPtr("updatedName"),
			expectedDescription:              cutil.GetPtr("Updated description"),
			expectedTenantID:                 &tenant2.ID,
			expectedInfrastructureProviderID: &ip2.ID,
			expectedSiteID:                   &site2.ID,
			expectedInstanceTypeID:           &instanceType2.ID,
			expectedVpcID:                    &vpc2.ID,
			expectedMachineID:                &machine2.ID,
			expectedHostname:                 cutil.GetPtr("updated.com"),
			expectedOperatingSystemID:        &operatingSystem2.ID,
			expectedIpxeScript:               cutil.GetPtr("updatedIpxe"),

			expectedUserData:          cutil.GetPtr("updatedUserData"),
			expectedInfinityRCRStatus: cutil.GetPtr("RESOURCE_GRANTED"),
			expectedStatus:            cutil.GetPtr(InstanceStatusReady),
			expectedPowerStatus:       cutil.GetPtr(InstancePowerStatusRebooting),
			expectedIsMissingOnSite:   cutil.GetPtr(true),
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := isd.Update(ctx, nil,
				InstanceUpdateInput{
					InstanceID: tc.id,
					InstanceUpdateCommonInput: InstanceUpdateCommonInput{
						Name:                                   tc.paramName,
						Description:                            tc.paramDescription,
						TenantID:                               tc.paramTenantID,
						InfrastructureProviderID:               tc.paramInfrastructureProviderID,
						SiteID:                                 tc.paramSiteID,
						InstanceTypeID:                         tc.paramInstanceTypeID,
						NetworkSecurityGroupID:                 tc.paramNetworkSecurityGroupID,
						NetworkSecurityGroupPropagationDetails: tc.paramNetworkSecurityGroupPropagationDetails,
						VpcID:                                  tc.paramVpcID,
						MachineID:                              tc.paramMachineID,
						ControllerInstanceID:                   tc.paramControlledInstanceID,
						Hostname:                               tc.paramHostname,
						OperatingSystemID:                      tc.paramOperatingSystemID,
						IpxeScript:                             tc.paramIpxeScript,
						AlwaysBootWithCustomIpxe:               tc.paramAlwaysBootWithCustomIpxe,
						PhoneHomeEnabled:                       tc.paramEnablePhoneHome,
						UserData:                               tc.paramUserData,
						Labels:                                 tc.paramLabels,
						IsUpdatePending:                        tc.paramIsUpdatePending,
						InfinityRCRStatus:                      tc.paramInfinityRCRStatus,
						Status:                                 tc.paramStatus,
						PowerStatus:                            tc.paramPowerStatus,
						IsMissingOnSite:                        tc.paramIsMissingOnSite,
						TpmEkCertificate:                       tc.paramTpmEkCertificate,
					},
				},
			)
			assert.Equal(t, tc.expectedError, err != nil)
			if err == nil {
				assert.EqualValues(t, tc.instance.ID, got.ID)

				if tc.expectedName != nil {
					assert.Equal(t, *tc.expectedName, got.Name)
				}
				if tc.expectedDescription != nil {
					assert.Equal(t, *tc.expectedDescription, *got.Description)
				}
				if len(got.Labels) > 0 {
					assert.EqualValues(t, tc.expectedLabels["region"], got.Labels["region"])
					assert.EqualValues(t, tc.expectedLabels["env"], got.Labels["env"])
				}

				assert.Equal(t, *tc.expectedTenantID, got.TenantID)
				assert.Equal(t, *tc.expectedInfrastructureProviderID, got.InfrastructureProviderID)
				assert.Equal(t, *tc.expectedSiteID, got.SiteID)
				assert.Equal(t, *tc.expectedInstanceTypeID, *got.InstanceTypeID)
				if tc.expectedNetworkSecurityGroupID != nil {
					assert.NotNil(t, got.NetworkSecurityGroupID)
					assert.Equal(t, *tc.expectedNetworkSecurityGroupID, *got.NetworkSecurityGroupID)
				}
				assert.Equal(t, *tc.expectedVpcID, got.VpcID)
				if tc.expectedMachineID != nil {
					assert.Equal(t, *tc.expectedMachineID, *got.MachineID)
				}
				if tc.expectedControllerInstanceID != nil {
					assert.Equal(t, *tc.expectedControllerInstanceID, *got.ControllerInstanceID)
				}

				if tc.expectedNetworkSecurityGroupPropagationDetails != nil {
					assert.Equal(t, tc.expectedNetworkSecurityGroupPropagationDetails.Details, got.NetworkSecurityGroupPropagationDetails.Details)
					assert.Equal(t, tc.expectedNetworkSecurityGroupPropagationDetails.Id, got.NetworkSecurityGroupPropagationDetails.Id)
					assert.Equal(t, tc.expectedNetworkSecurityGroupPropagationDetails.RelatedInstanceIds, got.NetworkSecurityGroupPropagationDetails.RelatedInstanceIds)
					assert.Equal(t, tc.expectedNetworkSecurityGroupPropagationDetails.UnpropagatedInstanceIds, got.NetworkSecurityGroupPropagationDetails.UnpropagatedInstanceIds)
					assert.Equal(t, tc.expectedNetworkSecurityGroupPropagationDetails.Status, got.NetworkSecurityGroupPropagationDetails.Status)
				}

				if tc.expectedNetworkSecurityGroupID != nil {
					assert.Equal(t, *tc.expectedNetworkSecurityGroupID, *got.NetworkSecurityGroupID)
				}

				if tc.expectedHostname != nil {
					assert.Equal(t, *tc.expectedHostname, *got.Hostname)
				}
				assert.Equal(t, *tc.expectedOperatingSystemID, *got.OperatingSystemID)
				if tc.expectedIpxeScript != nil {
					assert.Equal(t, *tc.expectedIpxeScript, *got.IpxeScript)
				}
				if tc.expectAlwaysBootWithCustomIpxe != nil {
					assert.Equal(t, *tc.expectAlwaysBootWithCustomIpxe, got.AlwaysBootWithCustomIpxe)
				}
				if tc.expectEnablePhoneHome != nil {
					assert.Equal(t, *tc.expectEnablePhoneHome, got.PhoneHomeEnabled)
				}
				if tc.expectedUserData != nil {
					assert.Equal(t, *tc.expectedUserData, *got.UserData)
				}

				if tc.expectIsUpdatePending != nil {
					assert.Equal(t, *tc.expectIsUpdatePending, got.IsUpdatePending)
				}

				assert.Equal(t, *tc.expectedStatus, got.Status)

				if tc.expectedPowerStatus != nil {
					assert.Equal(t, *tc.expectedPowerStatus, *got.PowerStatus)
				}

				if tc.expectedIsMissingOnSite != nil {
					assert.Equal(t, *tc.expectedIsMissingOnSite, got.IsMissingOnSite)
				}
				if tc.expectedTpmEkCertificate != nil {
					assert.Equal(t, *tc.expectedTpmEkCertificate, *got.TpmEkCertificate)
				}

				if got.Updated.String() == tc.instance.Updated.String() {
					t.Errorf("got.Updated = %v, want different value", got.Updated)
				}

				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
					_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
					assert.True(t, ok)
				}
			}
		})
	}
}

func TestInstanceSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant, site, "testNetworkSecurityGroup")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)
	dummyUUID := uuid.New()
	i1, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			NetworkSecurityGroupID:   &networkSecurityGroup.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			ControllerInstanceID:     &dummyUUID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{"label1": "value1"},
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
			NetworkSecurityGroupPropagationDetails: &NetworkSecurityGroupPropagationDetails{
				NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{},
			},
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i1)
	i2, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test2",
			Description:              cutil.GetPtr("Test description 2"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			NetworkSecurityGroupID:   &networkSecurityGroup.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			ControllerInstanceID:     &dummyUUID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i2)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                                        string
		id                                          uuid.UUID
		instance                                    *Instance
		paramDescription                            bool
		paramMachineID                              bool
		paramControlledInstanceID                   bool
		paramHostname                               bool
		paramOperatingSystemID                      bool
		paramIpxeScript                             bool
		paramRebootWithCustomIpxe                   bool
		paramUserData                               bool
		paramLabels                                 bool
		paramNetworkSecurityGroupID                 bool
		paramNetworkSecurityGroupPropagationDetails bool
		paramTpmEkCertificate                       bool

		expectedUpdate                                 bool
		expectedError                                  bool
		expectedName                                   *string
		expectedDescription                            *string
		expectedTenantID                               *uuid.UUID
		expectedInfrastructureProviderID               *uuid.UUID
		expectedSiteID                                 *uuid.UUID
		expectedInstanceTypeID                         *uuid.UUID
		expectedNetworkSecurityGroupID                 *string
		expectednetworkSecurityGroupPropagationDetails *corev1.NetworkSecurityGroupPropagationObjectStatus
		expectedVpcID                                  *uuid.UUID
		expectedMachineID                              *string
		expectedControllerInstanceID                   *uuid.UUID
		expectedHostname                               *string
		expectedOperatingSystemID                      *uuid.UUID
		expectedIpxeScript                             *string
		expectRebootWithCustomIpxe                     *bool
		expectedUserData                               *string
		expectedLabels                                 map[string]string
		expectedStatus                                 *string
		verifyChildSpanner                             bool
	}{
		{
			desc:                        "Can clear fields",
			id:                          i1.ID,
			instance:                    i1,
			paramDescription:            true,
			paramOperatingSystemID:      true,
			paramMachineID:              true,
			paramControlledInstanceID:   true,
			paramHostname:               true,
			paramIpxeScript:             true,
			paramRebootWithCustomIpxe:   true,
			paramUserData:               true,
			paramLabels:                 true,
			paramNetworkSecurityGroupID: true,
			paramNetworkSecurityGroupPropagationDetails: true,

			expectedUpdate:                   true,
			expectedError:                    false,
			expectedName:                     cutil.GetPtr("test1"),
			expectedDescription:              nil,
			expectedTenantID:                 &tenant.ID,
			expectedInfrastructureProviderID: &ip.ID,
			expectedSiteID:                   &site.ID,
			expectedInstanceTypeID:           &instanceType.ID,
			expectedNetworkSecurityGroupID:   nil,
			expectednetworkSecurityGroupPropagationDetails: nil,
			expectedVpcID:                &vpc.ID,
			expectedMachineID:            nil,
			expectedControllerInstanceID: nil,
			expectedHostname:             nil,
			expectedOperatingSystemID:    nil,
			expectedIpxeScript:           nil,
			expectRebootWithCustomIpxe:   nil,
			expectedUserData:             nil,
			expectedStatus:               cutil.GetPtr(InstanceStatusPending),
			verifyChildSpanner:           true,
			expectedLabels:               nil,
		},
		{
			desc:            "Error when attempting to clear unknown object",
			id:              dummyUUID,
			instance:        i1,
			paramHostname:   true,
			paramIpxeScript: true,
			paramUserData:   true,
			paramLabels:     true,

			expectedError:  true,
			expectedLabels: nil,
		},
		{
			desc:            "OK when nothing is cleared",
			id:              i2.ID,
			instance:        i2,
			paramHostname:   false,
			paramIpxeScript: false,
			paramUserData:   false,

			expectedError:                    false,
			expectedName:                     cutil.GetPtr("test2"),
			expectedDescription:              cutil.GetPtr("Test description 2"),
			expectedTenantID:                 &tenant.ID,
			expectedInfrastructureProviderID: &ip.ID,
			expectedSiteID:                   &site.ID,
			expectedInstanceTypeID:           &instanceType.ID,
			expectedNetworkSecurityGroupID:   &networkSecurityGroup.ID,
			expectedVpcID:                    &vpc.ID,
			expectedMachineID:                &machine.ID,
			expectedControllerInstanceID:     &dummyUUID,
			expectedHostname:                 cutil.GetPtr("test.com"),
			expectedOperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			expectedIpxeScript:               cutil.GetPtr("ipxe"),
			expectedUserData:                 cutil.GetPtr("userdata"),
			expectedStatus:                   cutil.GetPtr(InstanceStatusPending),
			expectedLabels:                   map[string]string{},
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := isd.Clear(
				ctx, nil,
				InstanceClearInput{
					InstanceID:                             tc.id,
					Description:                            tc.paramDescription,
					MachineID:                              tc.paramMachineID,
					ControllerInstanceID:                   tc.paramControlledInstanceID,
					Hostname:                               tc.paramHostname,
					OperatingSystemID:                      tc.paramOperatingSystemID,
					IpxeScript:                             tc.paramIpxeScript,
					UserData:                               tc.paramUserData,
					Labels:                                 tc.paramLabels,
					NetworkSecurityGroupID:                 tc.paramNetworkSecurityGroupID,
					NetworkSecurityGroupPropagationDetails: tc.paramNetworkSecurityGroupPropagationDetails,
					TpmEkCertificate:                       tc.paramTpmEkCertificate,
				},
			)
			assert.Equal(t, tc.expectedError, err != nil)
			if err == nil {
				assert.EqualValues(t, tc.instance.ID, got.ID)

				assert.Equal(t, *tc.expectedTenantID, got.TenantID)
				assert.Equal(t, *tc.expectedInfrastructureProviderID, got.InfrastructureProviderID)
				assert.Equal(t, *tc.expectedSiteID, got.SiteID)
				assert.Equal(t, *tc.expectedInstanceTypeID, *got.InstanceTypeID)
				assert.Equal(t, *tc.expectedVpcID, got.VpcID)

				if tc.paramNetworkSecurityGroupPropagationDetails {
					assert.Nil(t, got.NetworkSecurityGroupPropagationDetails)
				}

				if tc.expectedNetworkSecurityGroupID != nil {
					assert.NotNil(t, got.NetworkSecurityGroupID)
					assert.Equal(t, *tc.expectedNetworkSecurityGroupID, *got.NetworkSecurityGroupID)
				}

				if tc.expectedHostname != nil {
					assert.Equal(t, *tc.expectedHostname, *got.Hostname)
				}

				if tc.paramOperatingSystemID {
					assert.Nil(t, got.OperatingSystemID)
				} else if tc.expectedOperatingSystemID != nil {
					assert.NotNil(t, got.OperatingSystemID)
					assert.Equal(t, *tc.expectedOperatingSystemID, *got.OperatingSystemID)
				}

				if tc.expectedIpxeScript != nil {
					assert.Equal(t, *tc.expectedIpxeScript, *got.IpxeScript)
				}
				if tc.expectedUserData != nil {
					assert.Equal(t, *tc.expectedUserData, *got.UserData)
				}

				assert.Equal(t, tc.expectedLabels, got.Labels)

				if tc.expectedMachineID != nil {
					assert.Equal(t, *tc.expectedMachineID, *got.MachineID)
				}
				if tc.expectedControllerInstanceID != nil {
					assert.Equal(t, *tc.expectedControllerInstanceID, *got.ControllerInstanceID)
				}
				assert.Equal(t, *tc.expectedStatus, got.Status)

				if tc.expectedUpdate {
					assert.True(t, got.Updated.After(tc.instance.Updated))
				}
			}
		})
	}
}

func TestInstanceSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant, site, "testNetworkSecurityGroup")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)
	dummyUUID := uuid.New()
	i1, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test1",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			NetworkSecurityGroupID:   &networkSecurityGroup.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			ControllerInstanceID:     &dummyUUID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_RELEASED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 uuid.UUID
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object",
			id:                 i1.ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "delete non-existing object",
			id:            dummyUUID,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := isd.Delete(ctx, nil, tc.id)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := isd.GetByID(ctx, nil, tc.id, nil)
				assert.NotNil(t, err)
				assert.Nil(t, tmp)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestInstanceSQLDAO_CreateMultiple(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	networkSecurityGroup := testInstanceBuildNetworkSecurityGroup(t, dbSession, tenant, site, "testNetworkSecurityGroup")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []InstanceCreateInput
		expectError        bool
		expectedCount      int
		verifyChildSpanner bool
	}{
		{
			desc: "create batch of three instances",
			inputs: []InstanceCreateInput{
				{
					Name:                     "test-batch-1",
					Description:              cutil.GetPtr("Test batch description 1"),
					TenantID:                 tenant.ID,
					InfrastructureProviderID: ip.ID,
					SiteID:                   site.ID,
					InstanceTypeID:           &instanceType.ID,
					NetworkSecurityGroupID:   &networkSecurityGroup.ID,
					VpcID:                    vpc.ID,
					MachineID:                &machine.ID,
					Hostname:                 cutil.GetPtr("test1.com"),
					OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
					Status:                   InstanceStatusPending,
					PowerStatus:              cutil.GetPtr(InstancePowerStatusBootCompleted),
					IpxeScript:               cutil.GetPtr("ipxe1"),
					AlwaysBootWithCustomIpxe: true,
					PhoneHomeEnabled:         true,
					UserData:                 cutil.GetPtr("data1"),
					CreatedBy:                user.ID,
					Labels:                   map[string]string{"env": "test"},
				},
				{
					Name:                     "test-batch-2",
					TenantID:                 tenant.ID,
					InfrastructureProviderID: ip.ID,
					SiteID:                   site.ID,
					VpcID:                    vpc.ID,
					Status:                   InstanceStatusPending,
					CreatedBy:                user.ID,
					Labels:                   map[string]string{},
				},
				{
					Name:                     "test-batch-3",
					Description:              cutil.GetPtr("Test batch description 3"),
					TenantID:                 tenant.ID,
					InfrastructureProviderID: ip.ID,
					SiteID:                   site.ID,
					InstanceTypeID:           &instanceType.ID,
					VpcID:                    vpc.ID,
					Status:                   InstanceStatusReady,
					CreatedBy:                user.ID,
					Labels:                   map[string]string{"tier": "production"},
				},
			},
			expectError:        false,
			expectedCount:      3,
			verifyChildSpanner: true,
		},
		{
			desc:               "create batch with empty input",
			inputs:             []InstanceCreateInput{},
			expectError:        false,
			expectedCount:      0,
			verifyChildSpanner: false,
		},
		{
			desc: "create batch with single instance",
			inputs: []InstanceCreateInput{
				{
					Name:                     "test-single",
					TenantID:                 tenant.ID,
					InfrastructureProviderID: ip.ID,
					SiteID:                   site.ID,
					VpcID:                    vpc.ID,
					Status:                   InstanceStatusPending,
					CreatedBy:                user.ID,
					Labels:                   map[string]string{},
				},
			},
			expectError:   false,
			expectedCount: 1,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := isd.CreateMultiple(ctx, nil, tc.inputs)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Verify each created instance has a valid ID and timestamps
				// Also verify that results are returned in the same order as inputs
				for i, instance := range got {
					assert.NotEqual(t, uuid.Nil, instance.ID)
					assert.Equal(t, tc.inputs[i].Name, instance.Name, "result order should match input order")
					assert.Equal(t, tc.inputs[i].Status, instance.Status)
					assert.NotZero(t, instance.Created)
					assert.NotZero(t, instance.Updated)
				}
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestInstanceSQLDAO_CreateMultiple_ExceedsMaxBatchItems(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	isd := NewInstanceDAO(dbSession)

	// Create inputs exceeding MaxBatchItems
	inputs := make([]InstanceCreateInput, db.MaxBatchItems+1)
	for i := range inputs {
		inputs[i] = InstanceCreateInput{
			Name:   fmt.Sprintf("test-%d", i),
			Status: InstanceStatusPending,
		}
	}

	_, err := isd.CreateMultiple(ctx, nil, inputs)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "batch size")
	assert.Contains(t, err.Error(), "exceeds maximum allowed")
}

func TestInstanceSQLDAO_UpdateMultiple(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)

	// Create test instances
	i1, err := isd.Create(ctx, nil, InstanceCreateInput{
		Name:                     "test-update-1",
		TenantID:                 tenant.ID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                user.ID,
		Labels:                   map[string]string{},
	})
	assert.Nil(t, err)

	i2, err := isd.Create(ctx, nil, InstanceCreateInput{
		Name:                     "test-update-2",
		TenantID:                 tenant.ID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                user.ID,
		Labels:                   map[string]string{},
	})
	assert.Nil(t, err)

	i3, err := isd.Create(ctx, nil, InstanceCreateInput{
		Name:                     "test-update-3",
		TenantID:                 tenant.ID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                user.ID,
		Labels:                   map[string]string{},
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	// UpdateMultiple applies a single shared update mask to N instance
	// IDs. Each subtest sets the same fields on every targeted row;
	// heterogeneous per-row updates require separate calls.
	tests := []struct {
		desc               string
		input              InstanceUpdateMultipleInput
		expectError        bool
		expectedCount      int
		expectStatus       *string
		expectName         *string
		verifyChildSpanner bool
	}{
		{
			desc: "batch update three instances with shared patch",
			input: InstanceUpdateMultipleInput{
				InstanceIDs: []uuid.UUID{i1.ID, i2.ID, i3.ID},
				InstanceUpdateCommonInput: InstanceUpdateCommonInput{
					Status:            cutil.GetPtr(InstanceStatusReady),
					MachineID:         &machine.ID,
					OperatingSystemID: cutil.GetPtr(operatingSystem.ID),
					Labels:            map[string]string{"updated": "true"},
				},
			},
			expectError:        false,
			expectedCount:      3,
			expectStatus:       cutil.GetPtr(InstanceStatusReady),
			verifyChildSpanner: true,
		},
		{
			desc:          "batch update with empty input",
			input:         InstanceUpdateMultipleInput{},
			expectError:   false,
			expectedCount: 0,
		},
		{
			desc: "batch update single instance via slice of one",
			input: InstanceUpdateMultipleInput{
				InstanceIDs: []uuid.UUID{i1.ID},
				InstanceUpdateCommonInput: InstanceUpdateCommonInput{
					Status: cutil.GetPtr(InstanceStatusUpdating),
				},
			},
			expectError:   false,
			expectedCount: 1,
			expectStatus:  cutil.GetPtr(InstanceStatusUpdating),
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := isd.UpdateMultiple(ctx, nil, tc.input)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Result order must match input order; every row should
				// reflect the shared mask values.
				for i, instance := range got {
					assert.Equal(t, tc.input.InstanceIDs[i], instance.ID, "result order should match input order")
					if tc.expectStatus != nil {
						assert.Equal(t, *tc.expectStatus, instance.Status)
					}
					if tc.expectName != nil {
						assert.Equal(t, *tc.expectName, instance.Name)
					}
				}
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestInstanceSQLDAO_UpdateMultiple_ExceedsMaxBatchItems(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	isd := NewInstanceDAO(dbSession)

	// Create IDs exceeding MaxBatchItems
	ids := make([]uuid.UUID, db.MaxBatchItems+1)
	for i := range ids {
		ids[i] = uuid.New()
	}
	input := InstanceUpdateMultipleInput{
		InstanceIDs:               ids,
		InstanceUpdateCommonInput: InstanceUpdateCommonInput{Name: cutil.GetPtr("test")},
	}

	_, err := isd.UpdateMultiple(ctx, nil, input)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "batch size")
	assert.Contains(t, err.Error(), "exceeds maximum allowed")
}

// TestInstanceSQLDAO_UpdateMultiple_RejectsDuplicateIDs covers the
// pre-write guard against duplicate InstanceIDs in a single call.
// Without it, the post-fetch SELECT returns one row per unique ID
// while the input slice still counts duplicates, surfacing as a
// post-write count-mismatch with a partially applied batch.
func TestInstanceSQLDAO_UpdateMultiple_RejectsDuplicateIDs(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	isd := NewInstanceDAO(dbSession)

	dupID := uuid.New()
	input := InstanceUpdateMultipleInput{
		InstanceIDs:               []uuid.UUID{dupID, uuid.New(), dupID},
		InstanceUpdateCommonInput: InstanceUpdateCommonInput{Status: cutil.GetPtr(InstanceStatusReady)},
	}

	_, err := isd.UpdateMultiple(ctx, nil, input)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "duplicate instance id")
	assert.Contains(t, err.Error(), dupID.String())
}

func TestInstanceSQLDAO_GetAll_WithNames(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)

	// Create test instances
	i1, err := isd.Create(ctx, nil, InstanceCreateInput{
		Name:                     "instance-alpha",
		TenantID:                 tenant.ID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                user.ID,
		Labels:                   map[string]string{},
	})
	assert.Nil(t, err)

	_, err = isd.Create(ctx, nil, InstanceCreateInput{
		Name:                     "instance-beta",
		TenantID:                 tenant.ID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                user.ID,
		Labels:                   map[string]string{},
	})
	assert.Nil(t, err)

	_, err = isd.Create(ctx, nil, InstanceCreateInput{
		Name:                     "instance-gamma",
		TenantID:                 tenant.ID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                user.ID,
		Labels:                   map[string]string{},
	})
	assert.Nil(t, err)

	tests := []struct {
		desc          string
		filter        InstanceFilterInput
		expectedCount int
		expectedNames []string
	}{
		{
			desc: "filter by multiple names",
			filter: InstanceFilterInput{
				Names: []string{"instance-alpha", "instance-beta"},
			},
			expectedCount: 2,
			expectedNames: []string{"instance-alpha", "instance-beta"},
		},
		{
			desc: "filter by single name in Names array",
			filter: InstanceFilterInput{
				Names: []string{"instance-alpha"},
			},
			expectedCount: 1,
			expectedNames: []string{"instance-alpha"},
		},
		{
			desc: "filter by non-existent names",
			filter: InstanceFilterInput{
				Names: []string{"non-existent"},
			},
			expectedCount: 0,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			instances, total, err := isd.GetAll(ctx, nil, tc.filter, paginator.PageInput{}, nil)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedCount, len(instances))
			assert.Equal(t, tc.expectedCount, total)

			if tc.expectedCount > 0 {
				for i, instance := range instances {
					assert.Equal(t, tc.expectedNames[i], instance.Name)
				}
			}
		})
	}

	// Test that single Names filter with one element works
	t.Run("filter by single Names element", func(t *testing.T) {
		instances, total, err := isd.GetAll(ctx, nil, InstanceFilterInput{
			Names: []string{i1.Name},
		}, paginator.PageInput{}, nil)
		assert.Nil(t, err)
		assert.Equal(t, 1, len(instances))
		assert.Equal(t, 1, total)
		assert.Equal(t, i1.Name, instances[0].Name)
	})
}

// TestInstanceSQLDAO_UpdateMultiple_AllFields verifies that ALL fields in InstanceUpdateInput
// are correctly handled by UpdateMultiple. This test will fail if any field is missed.
func TestInstanceSQLDAO_UpdateMultiple_AllFields(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInstanceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcType"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, user.ID)
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	isd := NewInstanceDAO(dbSession)

	// Create an instance with minimal fields
	instance, err := isd.Create(ctx, nil, InstanceCreateInput{
		Name:                     "test-instance",
		TenantID:                 tenant.ID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		VpcID:                    vpc.ID,
		Status:                   InstanceStatusPending,
		CreatedBy:                user.ID,
	})
	assert.NoError(t, err)

	// Prepare new values for update
	newControllerInstanceID := uuid.New()

	// Update with ALL fields set to new values, applied as a shared
	// mask to a single-ID slice.
	input := InstanceUpdateMultipleInput{
		InstanceIDs: []uuid.UUID{instance.ID},
		InstanceUpdateCommonInput: InstanceUpdateCommonInput{
			Name:                     cutil.GetPtr("updated-instance-name"),
			Description:              cutil.GetPtr("updated description"),
			TenantID:                 &tenant.ID,
			InfrastructureProviderID: &ip.ID,
			SiteID:                   &site.ID,
			InstanceTypeID:           &instanceType.ID,
			// NetworkSecurityGroupID is a FK, skip it
			NetworkSecurityGroupPropagationDetails: &NetworkSecurityGroupPropagationDetails{
				FriendlyStatus: "propagated",
			},
			VpcID:                    &vpc.ID,
			MachineID:                &machine.ID,
			ControllerInstanceID:     &newControllerInstanceID,
			Hostname:                 cutil.GetPtr("new-hostname.example.com"),
			OperatingSystemID:        &operatingSystem.ID,
			IpxeScript:               cutil.GetPtr("new-ipxe-script"),
			AlwaysBootWithCustomIpxe: cutil.GetPtr(true),
			PhoneHomeEnabled:         cutil.GetPtr(true),
			UserData:                 cutil.GetPtr("new-userdata"),
			Labels:                   map[string]string{"env": "prod", "team": "platform"},
			IsUpdatePending:          cutil.GetPtr(true),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			TpmEkCertificate:         cutil.GetPtr("tpm-cert-data"),
			Status:                   cutil.GetPtr(InstanceStatusReady),
			PowerStatus:              cutil.GetPtr("on"),
			IsMissingOnSite:          cutil.GetPtr(true),
		},
	}

	results, err := isd.UpdateMultiple(ctx, nil, input)
	assert.NoError(t, err)
	assert.Len(t, results, 1)

	updated := results[0]

	// Verify ALL fields were updated correctly
	assert.Equal(t, instance.ID, updated.ID)
	assert.Equal(t, "updated-instance-name", updated.Name, "Name not updated")
	assert.Equal(t, "updated description", *updated.Description, "Description not updated")
	assert.Equal(t, tenant.ID, updated.TenantID, "TenantID not updated")
	assert.Equal(t, ip.ID, updated.InfrastructureProviderID, "InfrastructureProviderID not updated")
	assert.Equal(t, site.ID, updated.SiteID, "SiteID not updated")
	assert.Equal(t, &instanceType.ID, updated.InstanceTypeID, "InstanceTypeID not updated")
	assert.NotNil(t, updated.NetworkSecurityGroupPropagationDetails, "NetworkSecurityGroupPropagationDetails not updated")
	assert.Equal(t, vpc.ID, updated.VpcID, "VpcID not updated")
	assert.Equal(t, &machine.ID, updated.MachineID, "MachineID not updated")
	assert.Equal(t, &newControllerInstanceID, updated.ControllerInstanceID, "ControllerInstanceID not updated")
	assert.Equal(t, "new-hostname.example.com", *updated.Hostname, "Hostname not updated")
	assert.Equal(t, &operatingSystem.ID, updated.OperatingSystemID, "OperatingSystemID not updated")
	assert.Equal(t, "new-ipxe-script", *updated.IpxeScript, "IpxeScript not updated")
	assert.True(t, updated.AlwaysBootWithCustomIpxe, "AlwaysBootWithCustomIpxe not updated")
	assert.True(t, updated.PhoneHomeEnabled, "PhoneHomeEnabled not updated")
	assert.Equal(t, "new-userdata", *updated.UserData, "UserData not updated")
	assert.Equal(t, map[string]string{"env": "prod", "team": "platform"}, updated.Labels, "Labels not updated")
	assert.True(t, updated.IsUpdatePending, "IsUpdatePending not updated")
	assert.Equal(t, "RESOURCE_GRANTED", *updated.InfinityRCRStatus, "InfinityRCRStatus not updated")
	assert.Equal(t, "tpm-cert-data", *updated.TpmEkCertificate, "TpmEkCertificate not updated")
	assert.Equal(t, InstanceStatusReady, updated.Status, "Status not updated")
	assert.Equal(t, "on", *updated.PowerStatus, "PowerStatus not updated")
	assert.True(t, updated.IsMissingOnSite, "IsMissingOnSite not updated")
}

func TestInstance_GetSiteID(t *testing.T) {
	id := uuid.New()
	ctrlID := uuid.New()
	t.Run("falls back to ID when ControllerInstanceID is nil", func(t *testing.T) {
		i := &Instance{ID: id}
		got := i.GetSiteID()
		require.NotNil(t, got)
		assert.Equal(t, id, *got)
	})
	t.Run("uses ControllerInstanceID when set", func(t *testing.T) {
		i := &Instance{ID: id, ControllerInstanceID: &ctrlID}
		got := i.GetSiteID()
		require.NotNil(t, got)
		assert.Equal(t, ctrlID, *got)
	})
}

func TestInstance_ToReleaseRequestProto(t *testing.T) {
	id := uuid.New()
	i := &Instance{ID: id}
	req := i.ToReleaseRequestProto()
	require.NotNil(t, req)
	require.NotNil(t, req.Id)
	assert.Equal(t, id.String(), req.Id.Value)
	assert.Nil(t, req.Issue)
}
