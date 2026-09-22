// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
)

// reset the tables needed for Interface tests
func testInterfaceSetupSchema(t *testing.T, dbSession *db.Session) {
	// create User table
	err := dbSession.DB.ResetModel(context.Background(), (*User)(nil))
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
	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*InstanceType)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*IPBlock)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*Allocation)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*AllocationConstraint)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*Machine)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*Vpc)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*OperatingSystem)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*Instance)(nil))
	assert.Nil(t, err)
	// create domain table
	err = dbSession.DB.ResetModel(context.Background(), (*Domain)(nil))
	assert.Nil(t, err)
	// create VpcPrefix table
	err = dbSession.DB.ResetModel(context.Background(), (*VpcPrefix)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*Subnet)(nil))
	assert.Nil(t, err)
	// create MachineInterface
	err = dbSession.DB.ResetModel(context.Background(), (*MachineInterface)(nil))
	assert.Nil(t, err)
	// create Interface
	err = dbSession.DB.ResetModel(context.Background(), (*Interface)(nil))
	assert.Nil(t, err)
}

func TestInterfaceInlineRoutingProfile_ToProtoFromProto(t *testing.T) {
	var nilProfile *InterfaceInlineRoutingProfile
	assert.Nil(t, nilProfile.ToProto())

	emptyProfile := &InterfaceInlineRoutingProfile{}
	emptyProto := emptyProfile.ToProto()
	require.NotNil(t, emptyProto)
	assert.NotNil(t, emptyProto.AllowedAnycastPrefixes)
	assert.Empty(t, emptyProto.AllowedAnycastPrefixes)

	profile := &InterfaceInlineRoutingProfile{
		AllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
	}
	protoProfile := profile.ToProto()
	require.NotNil(t, protoProfile)
	require.Len(t, protoProfile.AllowedAnycastPrefixes, 2)
	assert.Equal(t, "192.0.2.0/24", protoProfile.AllowedAnycastPrefixes[0].Prefix)
	assert.Equal(t, "2001:db8::/64", protoProfile.AllowedAnycastPrefixes[1].Prefix)

	var roundTrip InterfaceInlineRoutingProfile
	roundTrip.FromProto(protoProfile)
	assert.Equal(t, profile.AllowedAnycastPrefixes, roundTrip.AllowedAnycastPrefixes)

	roundTrip.FromProto(nil)
	assert.Nil(t, roundTrip.AllowedAnycastPrefixes)

	var fromProto InterfaceInlineRoutingProfile
	fromProto.FromProto(&corev1.InstanceInterfaceRoutingProfile{
		AllowedAnycastPrefixes: []*corev1.PrefixFilterPolicyEntry{
			{Prefix: "198.51.100.0/24"},
			{Prefix: "2001:db8:1::/64"},
		},
	})
	assert.Equal(t, []string{"198.51.100.0/24", "2001:db8:1::/64"}, fromProto.AllowedAnycastPrefixes)
}

func TestInterface_EthernetKey(t *testing.T) {
	vpcID := uuid.New()
	familyMode := InterfaceVpcIPFamilyModeIPv4Only
	deviceInstance := 0

	var base Interface
	base.VpcID = &vpcID
	base.VpcIPFamilyMode = &familyMode
	base.Device = cutil.GetPtr("device")
	base.DeviceInstance = &deviceInstance
	base.IsPhysical = true

	sameVpcID := vpcID
	sameFamilyMode := familyMode
	sameDeviceInstance := deviceInstance
	same := base
	same.VpcID = &sameVpcID
	same.VpcIPFamilyMode = &sameFamilyMode
	same.Device = cutil.GetPtr("device")
	same.DeviceInstance = &sameDeviceInstance
	same.VpcPrefixID = cutil.GetPtr(uuid.New())
	assert.Equal(t, base.EthernetInterfaceKey(), same.EthernetInterfaceKey(), "resolved prefixes must not change VPC-selector identity")

	missingDeviceInstance := base
	missingDeviceInstance.DeviceInstance = nil
	assert.NotEqual(t, base.EthernetInterfaceKey(), missingDeviceInstance.EthernetInterfaceKey())

	emptyProfile := base
	emptyProfile.InlineRoutingProfile = &InterfaceInlineRoutingProfile{}
	assert.NotEqual(t, base.EthernetInterfaceKey(), emptyProfile.EthernetInterfaceKey())

	differentVpc := base
	differentVpc.VpcID = cutil.GetPtr(uuid.New())
	assert.NotEqual(t, base.EthernetInterfaceKey(), differentVpc.EthernetInterfaceKey())

	virtualFunction := base
	virtualFunction.IsPhysical = false
	virtualFunction.VirtualFunctionID = cutil.GetPtr(1)
	assert.NotEqual(t, base.EthernetInterfaceKey(), virtualFunction.EthernetInterfaceKey(), "a virtual function must not match a physical function")

	otherVirtualFunction := virtualFunction
	otherVirtualFunction.VirtualFunctionID = cutil.GetPtr(2)
	assert.NotEqual(t, virtualFunction.EthernetInterfaceKey(), otherVirtualFunction.EthernetInterfaceKey())
}

func TestInterfaceSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInterfaceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType.ID), cutil.GetPtr("mcTypeTest"))
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
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
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
	assert.NotNil(t, i1)
	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", cutil.GetPtr(user.ID))
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", cutil.GetPtr(user.ID))

	ssd := NewSubnetDAO(dbSession)
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	subnet, err := ssd.Create(ctx, nil, SubnetCreateInput{
		Name:                       "test",
		Description:                cutil.GetPtr("test"),
		Org:                        "test",
		SiteID:                     site.ID,
		VpcID:                      vpc.ID,
		DomainID:                   &domain.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: &dummyUUID,
		RoutingType:                &ipv4Block.RoutingType,
		IPv4Prefix:                 &ipv4Prefix,
		IPv4Gateway:                &ipv4Gateway,
		IPv4BlockID:                &ipv4Block.ID,
		IPv6Prefix:                 &ipv6Prefix,
		IPv6Gateway:                &ipv6Gateway,
		IPv6BlockID:                &ipv6Block.ID,
		PrefixLength:               8,
		Status:                     SubnetStatusPending,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, subnet)

	vpsd := NewVpcPrefixDAO(dbSession)
	vpcPrefix, err := vpsd.Create(ctx, nil, VpcPrefixCreateInput{
		Name:         "VpcPrefix-1",
		TenantOrg:    "test",
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipv4Block.ID,
		Prefix:       ipv4Prefix,
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vpcPrefix)

	ifcd := NewInterfaceDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		iss                []Interface
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one with subnet",
			iss: []Interface{
				{
					ID:                 uuid.New(),
					InstanceID:         i1.ID,
					SubnetID:           &subnet.ID,
					RequestedIpAddress: cutil.GetPtr("192.0.2.10"),
					IsPhysical:         true,
					Status:             InterfaceStatusPending,
					CreatedBy:          user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create one with vpcprefix",
			iss: []Interface{
				{
					ID:                 uuid.New(),
					InstanceID:         i1.ID,
					VpcPrefixID:        &vpcPrefix.ID,
					RequestedIpAddress: cutil.GetPtr("192.0.2.11"),
					InlineRoutingProfile: &InterfaceInlineRoutingProfile{
						AllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
					},
					IsPhysical: false,
					Status:     InterfaceStatusPending,
					CreatedBy:  user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create one with vpc selection intent",
			iss: []Interface{
				{
					ID:              uuid.New(),
					InstanceID:      i1.ID,
					VpcID:           &vpc.ID,
					VpcIPFamilyMode: cutil.GetPtr(InterfaceVpcIPFamilyModeIPv4Only),
					IsPhysical:      false,
					Status:          InterfaceStatusPending,
					CreatedBy:       user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create one with device and device instance",
			iss: []Interface{
				{
					ID:             uuid.New(),
					InstanceID:     i1.ID,
					VpcPrefixID:    &vpcPrefix.ID,
					IsPhysical:     true,
					Device:         cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
					DeviceInstance: cutil.GetPtr(0),
					Status:         InterfaceStatusPending,
					CreatedBy:      user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create one with virtual function id",
			iss: []Interface{
				{
					ID:                uuid.New(),
					InstanceID:        i1.ID,
					VpcPrefixID:       &vpcPrefix.ID,
					IsPhysical:        false,
					VirtualFunctionID: cutil.GetPtr(1),
					Status:            InterfaceStatusPending,
					CreatedBy:         user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "failed create due to foreign key on instance",
			iss: []Interface{
				{
					ID:         uuid.New(),
					InstanceID: uuid.New(),
					SubnetID:   &subnet.ID,
					IsPhysical: true,
					Status:     InterfaceStatusPending,
					CreatedBy:  user.ID,
				},
			},
			expectError: true,
		},
		{
			desc: "failed create due to foreign key on subnet",
			iss: []Interface{
				{
					ID:         uuid.New(),
					InstanceID: i1.ID,
					SubnetID:   cutil.GetPtr(uuid.New()),
					IsPhysical: true,
					Status:     InterfaceStatusPending,
					CreatedBy:  user.ID,
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.iss {

				input := InterfaceCreateInput{
					InstanceID:           i.InstanceID,
					SubnetID:             i.SubnetID,
					VpcID:                i.VpcID,
					VpcIPFamilyMode:      i.VpcIPFamilyMode,
					VpcPrefixID:          i.VpcPrefixID,
					Device:               i.Device,
					DeviceInstance:       i.DeviceInstance,
					IsPhysical:           i.IsPhysical,
					VirtualFunctionID:    i.VirtualFunctionID,
					RequestedIpAddress:   i.RequestedIpAddress,
					InlineRoutingProfile: i.InlineRoutingProfile,
					Status:               i.Status,
					CreatedBy:            i.CreatedBy,
				}

				got, err := ifcd.Create(ctx, nil, input)
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, got)
					persisted, err := ifcd.GetByID(ctx, nil, got.ID, nil)
					require.NoError(t, err)
					require.NotNil(t, persisted)
					if i.Device != nil {
						assert.Equal(t, *i.Device, *got.Device)
						assert.Equal(t, *i.Device, *persisted.Device)
					}
					if i.DeviceInstance != nil {
						assert.Equal(t, *i.DeviceInstance, *got.DeviceInstance)
						assert.Equal(t, *i.DeviceInstance, *persisted.DeviceInstance)
					}
					if i.VirtualFunctionID != nil {
						assert.Equal(t, *i.VirtualFunctionID, *got.VirtualFunctionID)
						assert.Equal(t, *i.VirtualFunctionID, *persisted.VirtualFunctionID)
					}
					assert.Equal(t, i.RequestedIpAddress, got.RequestedIpAddress)
					assert.Equal(t, i.RequestedIpAddress, persisted.RequestedIpAddress)
					assert.Equal(t, i.InlineRoutingProfile, got.InlineRoutingProfile)
					assert.Equal(t, i.InlineRoutingProfile, persisted.InlineRoutingProfile)
					assert.Equal(t, i.VpcID, got.VpcID)
					assert.Equal(t, i.VpcID, persisted.VpcID)
					assert.Equal(t, i.VpcIPFamilyMode, got.VpcIPFamilyMode)
					assert.Equal(t, i.VpcIPFamilyMode, persisted.VpcIPFamilyMode)
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

func TestInterfaceSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInterfaceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType.ID), cutil.GetPtr("mcTypeTest"))
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
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
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
	assert.NotNil(t, i1)
	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", &user.ID)
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", &user.ID)

	ssd := NewSubnetDAO(dbSession)
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	subnet, err := ssd.Create(ctx, nil, SubnetCreateInput{
		Name:                       "test",
		Description:                cutil.GetPtr("test"),
		Org:                        "test",
		SiteID:                     site.ID,
		VpcID:                      vpc.ID,
		DomainID:                   &domain.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: &dummyUUID,
		RoutingType:                &ipv4Block.RoutingType,
		IPv4Prefix:                 &ipv4Prefix,
		IPv4Gateway:                &ipv4Gateway,
		IPv4BlockID:                &ipv4Block.ID,
		IPv6Prefix:                 &ipv6Prefix,
		IPv6Gateway:                &ipv6Gateway,
		IPv6BlockID:                &ipv6Block.ID,
		PrefixLength:               8,
		Status:                     SubnetStatusPending,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, subnet)

	vpsd := NewVpcPrefixDAO(dbSession)

	vpcPrefix, err := vpsd.Create(ctx, nil, VpcPrefixCreateInput{
		Name:         "VpcPrefix-1",
		TenantOrg:    "test",
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipv4Block.ID,
		Prefix:       ipv4Prefix,
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    user.ID,
	})

	assert.Nil(t, err)
	assert.NotNil(t, vpcPrefix)

	ifcd := NewInterfaceDAO(dbSession)

	input1 := InterfaceCreateInput{
		InstanceID: i1.ID,
		SubnetID:   &subnet.ID,
		IsPhysical: true,
		Status:     InterfaceStatusPending,
		CreatedBy:  user.ID,
	}

	ifc, err := ifcd.Create(ctx, nil, input1)
	assert.Nil(t, err)
	assert.NotNil(t, ifc)

	input2 := InterfaceCreateInput{
		InstanceID:      i1.ID,
		VpcID:           &vpc.ID,
		VpcIPFamilyMode: cutil.GetPtr(InterfaceVpcIPFamilyModeIPv4Only),
		VpcPrefixID:     &vpcPrefix.ID,
		Device:          cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
		DeviceInstance:  cutil.GetPtr(0),
		IsPhysical:      true,
		Status:          InterfaceStatusPending,
		CreatedBy:       user.ID,
	}
	ifc1, err := ifcd.Create(ctx, nil, input2)
	assert.Nil(t, err)
	assert.NotNil(t, ifc1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 uuid.UUID
		instance           *Instance
		paramRelations     []string
		expectedError      bool
		expectedInstance   bool
		expectedSubnet     bool
		expectedVpc        bool
		expectVpcPrefix    bool
		expectedDevice     bool
		expectedIsPhysical bool
		verifyChildSpanner bool
	}{
		{
			desc:               "success when found in case of subnet",
			id:                 ifc.ID,
			paramRelations:     nil,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:               "success when found in case of vpcprefix",
			id:                 ifc1.ID,
			paramRelations:     nil,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:           "fails when not found",
			id:             uuid.New(),
			paramRelations: nil,
			expectedError:  true,
		},
		{
			desc:               "success with subnet relations",
			id:                 ifc.ID,
			paramRelations:     []string{InstanceRelationName, SubnetRelationName, MachineInterfaceRelationName},
			expectedError:      false,
			expectedInstance:   true,
			expectedSubnet:     true,
			expectedIsPhysical: true,
		},
		{
			desc:               "success with vpcprefix relations",
			id:                 ifc1.ID,
			paramRelations:     []string{InstanceRelationName, MachineInterfaceRelationName, VpcRelationName, VpcPrefixRelationName},
			expectedError:      false,
			expectedInstance:   true,
			expectedVpc:        true,
			expectVpcPrefix:    true,
			expectedDevice:     true,
			expectedIsPhysical: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := ifcd.GetByID(ctx, nil, tc.id, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if err == nil {
				assert.EqualValues(t, tc.id, got.ID)
				if tc.expectedInstance {
					assert.EqualValues(t, i1.ID, got.Instance.ID)
				}
				if tc.expectedSubnet {
					assert.EqualValues(t, subnet.ID, *got.SubnetID)
				}
				if tc.expectedVpc {
					require.NotNil(t, got.Vpc)
					assert.EqualValues(t, vpc.ID, got.Vpc.ID)
				}
				if tc.expectedIsPhysical {
					assert.EqualValues(t, ifc.IsPhysical, got.IsPhysical)
				}
				if tc.expectVpcPrefix {
					assert.EqualValues(t, vpcPrefix.ID, *got.VpcPrefixID)
				}
				if tc.expectedDevice {
					assert.EqualValues(t, *ifc1.Device, *got.Device)
					assert.EqualValues(t, *ifc1.DeviceInstance, *got.DeviceInstance)
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

func TestInterfaceSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInterfaceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType.ID), cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)
	dummyUUID := uuid.New()

	totalCount := 30
	instances := []Instance{}
	for i := 0; i < totalCount/2; i++ {
		instance, err := isd.Create(
			ctx, nil,
			InstanceCreateInput{
				Name:                     "test1",
				TenantID:                 tenant.ID,
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
				Status:                   InstanceStatusPending,
				CreatedBy:                user.ID,
			},
		)
		assert.NoError(t, err)
		instances = append(instances, *instance)
	}
	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", cutil.GetPtr(user.ID))
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", cutil.GetPtr(user.ID))

	ssd := NewSubnetDAO(dbSession)
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	subnet1, err := ssd.Create(ctx, nil, SubnetCreateInput{
		Name:                       "test",
		Description:                cutil.GetPtr("test"),
		Org:                        "test",
		SiteID:                     site.ID,
		VpcID:                      vpc.ID,
		DomainID:                   &domain.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: &dummyUUID,
		RoutingType:                &ipv4Block.RoutingType,
		IPv4Prefix:                 &ipv4Prefix,
		IPv4Gateway:                &ipv4Gateway,
		IPv4BlockID:                &ipv4Block.ID,
		IPv6Prefix:                 &ipv6Prefix,
		IPv6Gateway:                &ipv6Gateway,
		IPv6BlockID:                &ipv6Block.ID,
		PrefixLength:               8,
		Status:                     SubnetStatusPending,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, subnet1)
	subnet2, err := ssd.Create(ctx, nil, SubnetCreateInput{
		Name:                       "test2",
		Description:                cutil.GetPtr("test"),
		Org:                        "test",
		SiteID:                     site.ID,
		VpcID:                      vpc.ID,
		DomainID:                   &domain.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: &dummyUUID,
		RoutingType:                &ipv4Block.RoutingType,
		IPv4Prefix:                 &ipv4Prefix,
		IPv4Gateway:                &ipv4Gateway,
		IPv4BlockID:                &ipv4Block.ID,
		IPv6Prefix:                 &ipv6Prefix,
		IPv6Gateway:                &ipv6Gateway,
		IPv6BlockID:                &ipv6Block.ID,
		PrefixLength:               8,
		Status:                     SubnetStatusPending,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, subnet2)

	vpsd := NewVpcPrefixDAO(dbSession)
	vpcPrefix, err := vpsd.Create(ctx, nil, VpcPrefixCreateInput{
		Name:         "VpcPrefix-1",
		TenantOrg:    "test",
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipv4Block.ID,
		Prefix:       ipv4Prefix,
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vpcPrefix)

	vpcPrefix1, err := vpsd.Create(ctx, nil, VpcPrefixCreateInput{
		Name:         "VpcPrefix-2",
		TenantOrg:    "test",
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipv4Block.ID,
		Prefix:       ipv4Prefix,
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vpcPrefix1)

	ifcd := NewInterfaceDAO(dbSession)

	instance1Subnets := []Interface{}
	instance2Subnets := []Interface{}
	instance1VpcPrefixes := []Interface{}
	instance2VpcPrefixes := []Interface{}

	for i := 0; i < totalCount/2; i++ {
		instance1Subnet, err := ifcd.Create(ctx, nil, InterfaceCreateInput{InstanceID: instances[i].ID, SubnetID: &subnet1.ID, IsPhysical: true, Status: InterfaceStatusPending, CreatedBy: user.ID})
		assert.NoError(t, err)

		instance1Subnets = append(instance1Subnets, *instance1Subnet)

		// Create one with device and device instance
		// Only the first one is physical
		device := cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller")
		deviceInstance := cutil.GetPtr(i)
		IsPhysical := false
		if i == 0 {
			IsPhysical = true
		}

		instance1VpcPrefix, err := ifcd.Create(ctx, nil, InterfaceCreateInput{InstanceID: instances[i].ID, VpcPrefixID: &vpcPrefix.ID, Device: device, DeviceInstance: deviceInstance, IsPhysical: IsPhysical, Status: InterfaceStatusPending, CreatedBy: user.ID})
		assert.NoError(t, err)

		instance1VpcPrefixes = append(instance1VpcPrefixes, *instance1VpcPrefix)

		instance2Subnet, err := ifcd.Create(ctx, nil, InterfaceCreateInput{InstanceID: instances[i].ID, SubnetID: &subnet2.ID, Status: InterfaceStatusPending, CreatedBy: user.ID})
		assert.NoError(t, err)

		instance2Subnets = append(instance2Subnets, *instance2Subnet)

		instance2VpcPrefix, err := ifcd.Create(ctx, nil, InterfaceCreateInput{InstanceID: instances[i].ID, VpcPrefixID: &vpcPrefix1.ID, Status: InterfaceStatusPending, CreatedBy: user.ID})
		assert.NoError(t, err)

		instance2VpcPrefixes = append(instance2VpcPrefixes, *instance2VpcPrefix)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)
	_, err = ifcd.Update(ctx, nil, InterfaceUpdateInput{
		InterfaceID:          instance1VpcPrefixes[0].ID,
		SecondaryVpcPrefixID: &vpcPrefix1.ID,
	})
	require.NoError(t, err)

	// Create separate interfaces with IP addresses for IP filtering tests (don't modify existing ones)
	ifcWithIP1, err := ifcd.Create(ctx, nil, InterfaceCreateInput{InstanceID: instances[0].ID, SubnetID: &subnet1.ID, IsPhysical: false, Status: InterfaceStatusPending, CreatedBy: user.ID})
	assert.NoError(t, err)
	_, err = ifcd.Update(ctx, nil, InterfaceUpdateInput{
		InterfaceID: ifcWithIP1.ID,
		IpAddresses: []string{"192.168.1.100", "10.0.0.50"},
	})
	assert.NoError(t, err)

	ifcWithIP2, err := ifcd.Create(ctx, nil, InterfaceCreateInput{InstanceID: instances[1].ID, SubnetID: &subnet1.ID, IsPhysical: false, Status: InterfaceStatusPending, CreatedBy: user.ID})
	assert.NoError(t, err)
	_, err = ifcd.Update(ctx, nil, InterfaceUpdateInput{
		InterfaceID: ifcWithIP2.ID,
		IpAddresses: []string{"192.168.1.101", "172.16.0.10"},
	})
	assert.NoError(t, err)

	ifcWithIP3, err := ifcd.Create(ctx, nil, InterfaceCreateInput{InstanceID: instances[2].ID, SubnetID: &subnet2.ID, IsPhysical: false, Status: InterfaceStatusPending, CreatedBy: user.ID})
	assert.NoError(t, err)
	_, err = ifcd.Update(ctx, nil, InterfaceUpdateInput{
		InterfaceID: ifcWithIP3.ID,
		IpAddresses: []string{"10.0.0.50", "172.16.0.20"},
	})
	assert.NoError(t, err)

	tests := []struct {
		desc               string
		InstanceID         *uuid.UUID
		SubnetID           *uuid.UUID
		VpcPrefixID        *uuid.UUID
		Device             *string
		DeviceInstance     *int
		IsPhysical         *bool
		status             *string
		IPAddresses        []string
		offset             *int
		limit              *int
		orderBy            *paginator.OrderBy
		firstEntry         *Interface
		expectedCount      int
		expectedTotal      *int
		expectedError      bool
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with Instance ID filter returns objects",
			InstanceID:         &instances[0].ID,
			expectedCount:      5, // 4 original + 1 IP address test interface
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "GetAll with Subnet ID filter returns objects",
			SubnetID:      &subnet1.ID,
			expectedCount: totalCount/2 + 2, // +2 for IP address test interfaces
			expectedError: false,
		},
		{
			desc:          "GetAll with VpcPrefix ID filter returns primary and secondary objects",
			VpcPrefixID:   &vpcPrefix1.ID,
			expectedCount: totalCount/2 + 1,
			expectedError: false,
		},
		{
			desc:          "GetAll with Device filter returns objects",
			Device:        cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:           "GetAll with DeviceInstance filter returns objects",
			DeviceInstance: cutil.GetPtr(0),
			expectedCount:  1,
			expectedError:  false,
		},
		{
			desc:          "GetAll with IsPhysical filter returns objects",
			IsPhysical:    cutil.GetPtr(true),
			expectedCount: totalCount/2 + 1,
			expectedError: false,
		},
		{
			desc:          "GetAll with no filters returns objects",
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount*2 + 3), // +3 for IP address test interfaces
			expectedError: false,
		},
		{
			desc:           "GetAll with relation returns objects",
			expectedCount:  paginator.DefaultLimit,
			expectedTotal:  cutil.GetPtr(totalCount*2 + 3), // +3 for IP address test interfaces
			expectedError:  false,
			paramRelations: []string{InstanceRelationName, SubnetRelationName, MachineInterfaceRelationName, VpcPrefixRelationName},
		},
		{
			desc:          "GetAll with limit returns objects",
			SubnetID:      &subnet1.ID,
			offset:        cutil.GetPtr(0),
			limit:         cutil.GetPtr(5),
			expectedCount: 5,
			expectedTotal: cutil.GetPtr(totalCount/2 + 2), // +2 for IP address test interfaces
			expectedError: false,
		},
		{
			desc:          "GetAll with offset returns objects",
			SubnetID:      &subnet2.ID,
			offset:        cutil.GetPtr(5),
			expectedCount: 11,                             // totalCount/2 + 1 - 5 offset = 11
			expectedTotal: cutil.GetPtr(totalCount/2 + 1), // +1 for IP address test interface
			expectedError: false,
		},
		{
			desc:     "GetAll with order by returns objects",
			SubnetID: &subnet1.ID,
			orderBy: &paginator.OrderBy{
				Field: "created",
				Order: paginator.OrderAscending,
			},
			firstEntry:    &instance1Subnets[0],
			expectedCount: totalCount/2 + 2, // +2 for IP address test interfaces
			expectedTotal: cutil.GetPtr(totalCount/2 + 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with InterfaceStatusPending status returns objects",
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount*2 + 3), // +3 for IP address test interfaces
			expectedError: false,
			status:        cutil.GetPtr(InterfaceStatusPending),
		},
		{
			desc:          "GetAll with InterfaceStatusError status returns no objects",
			expectedCount: 0,
			expectedTotal: cutil.GetPtr(0),
			expectedError: false,
			status:        cutil.GetPtr(InterfaceStatusError),
		},
		{
			desc:          "GetAll with single IPAddress filter returns matching interfaces",
			IPAddresses:   []string{"192.168.1.100"},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc:          "GetAll with IPAddress filter matching multiple interfaces returns all matches",
			IPAddresses:   []string{"10.0.0.50"},
			expectedCount: 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with multiple IPAddresses filter returns interfaces with any matching IP",
			IPAddresses:   []string{"192.168.1.100", "172.16.0.10"},
			expectedCount: 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with non-existent IPAddress filter returns no objects",
			IPAddresses:   []string{"255.255.255.255"},
			expectedCount: 0,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			var insIDs []uuid.UUID
			if tc.InstanceID != nil {
				insIDs = []uuid.UUID{*tc.InstanceID}
			}

			filterInput := InterfaceFilterInput{
				InstanceIDs:    insIDs,
				SubnetID:       tc.SubnetID,
				VpcPrefixID:    tc.VpcPrefixID,
				Device:         tc.Device,
				DeviceInstance: tc.DeviceInstance,
				IsPhysical:     tc.IsPhysical,
				IPAddresses:    tc.IPAddresses,
			}

			if tc.status != nil {
				filterInput.Statuses = []string{*tc.status}
			}

			page := paginator.PageInput{
				Offset:  tc.offset,
				Limit:   tc.limit,
				OrderBy: tc.orderBy,
			}

			got, total, err := ifcd.GetAll(ctx, nil, filterInput, page, tc.paramRelations)

			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
				assert.Equal(t, tc.expectedCount, len(got))

				if len(tc.paramRelations) > 0 {
					assert.NotNil(t, got[0].Subnet)
				}
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.firstEntry != nil {
				assert.Equal(t, *tc.firstEntry, got[0])
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

func TestInterfaceSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInterfaceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType.ID), cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)

	instance, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test1",
			TenantID:                 tenant.ID,
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
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.Nil(t, err)

	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", cutil.GetPtr(user.ID))
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", cutil.GetPtr(user.ID))
	dummyUUID := uuid.New()

	ssd := NewSubnetDAO(dbSession)
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	subnet, err := ssd.Create(ctx, nil, SubnetCreateInput{
		Name:                       "test",
		Description:                cutil.GetPtr("test"),
		Org:                        "test",
		SiteID:                     site.ID,
		VpcID:                      vpc.ID,
		DomainID:                   &domain.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: &dummyUUID,
		RoutingType:                &ipv4Block.RoutingType,
		IPv4Prefix:                 &ipv4Prefix,
		IPv4Gateway:                &ipv4Gateway,
		IPv4BlockID:                &ipv4Block.ID,
		IPv6Prefix:                 &ipv6Prefix,
		IPv6Gateway:                &ipv6Gateway,
		IPv6BlockID:                &ipv6Block.ID,
		PrefixLength:               8,
		Status:                     SubnetStatusPending,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)

	ifcd := NewInterfaceDAO(dbSession)
	requestedIpAddress := cutil.GetPtr("192.0.2.11")
	routingProfile := &InterfaceInlineRoutingProfile{
		AllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
	}
	ifc, err := ifcd.Create(ctx, nil, InterfaceCreateInput{
		InstanceID:           instance.ID,
		SubnetID:             &subnet.ID,
		IsPhysical:           true,
		RequestedIpAddress:   requestedIpAddress,
		InlineRoutingProfile: routingProfile,
		Status:               InterfaceStatusPending,
		CreatedBy:            user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, ifc)
	secondaryVpcPrefix, err := NewVpcPrefixDAO(dbSession).Create(ctx, nil, VpcPrefixCreateInput{
		Name:         "secondary-vpc-prefix",
		TenantOrg:    "test",
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipv4Block.ID,
		Prefix:       "192.0.3.0/24",
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    user.ID,
	})
	require.NoError(t, err)
	_, err = ifcd.Update(ctx, nil, InterfaceUpdateInput{
		InterfaceID:          ifc.ID,
		SecondaryVpcPrefixID: &secondaryVpcPrefix.ID,
	})
	require.NoError(t, err)

	badSession, err := db.NewSession(context.Background(), "localhost", 1234, "postgres", "postgres", "postgres", "")
	assert.Nil(t, err)
	badDAO := NewInterfaceDAO(badSession)

	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		dao                InterfaceDAO
		input              InterfaceClearInput
		expectError        bool
		expectSecondaryID  *uuid.UUID
		expectRequestedIP  *string
		expectRouting      *InterfaceInlineRoutingProfile
		verifyChildSpanner bool
	}{
		{
			desc:               "error clearing requested ip address",
			dao:                badDAO,
			input:              InterfaceClearInput{InterfaceID: ifc.ID, RequestedIpAddress: true},
			expectError:        true,
			expectSecondaryID:  &secondaryVpcPrefix.ID,
			expectRequestedIP:  requestedIpAddress,
			expectRouting:      routingProfile,
			verifyChildSpanner: true,
		},
		{
			desc:               "can clear routing profile",
			dao:                ifcd,
			input:              InterfaceClearInput{InterfaceID: ifc.ID, InlineRoutingProfile: true},
			expectError:        false,
			expectSecondaryID:  &secondaryVpcPrefix.ID,
			expectRequestedIP:  requestedIpAddress,
			expectRouting:      nil,
			verifyChildSpanner: true,
		},
		{
			desc:               "can clear requested ip address",
			dao:                ifcd,
			input:              InterfaceClearInput{InterfaceID: ifc.ID, RequestedIpAddress: true},
			expectError:        false,
			expectSecondaryID:  &secondaryVpcPrefix.ID,
			expectRequestedIP:  nil,
			expectRouting:      nil,
			verifyChildSpanner: true,
		},
		{
			desc:               "can clear secondary VPC prefix ID",
			dao:                ifcd,
			input:              InterfaceClearInput{InterfaceID: ifc.ID, SecondaryVpcPrefixID: true},
			expectError:        false,
			expectSecondaryID:  nil,
			expectRequestedIP:  nil,
			expectRouting:      nil,
			verifyChildSpanner: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := tc.dao.Clear(ctx, nil, tc.input)
			require.Equal(t, tc.expectError, err != nil, err)

			if tc.expectError {
				return
			}

			assert.Equal(t, tc.expectRequestedIP, got.RequestedIpAddress)
			assert.Equal(t, tc.expectRouting, got.InlineRoutingProfile)
			assert.Equal(t, tc.expectSecondaryID, got.SecondaryVpcPrefixID)

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestInterfaceSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInterfaceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType.ID), cutil.GetPtr("mcTypeTest"))
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
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
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
	assert.NotNil(t, i1)
	i2, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test2",
			TenantID:                 tenant.ID,
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
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i2)
	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", cutil.GetPtr(user.ID))
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", cutil.GetPtr(user.ID))

	ssd := NewSubnetDAO(dbSession)
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	subnet1, err := ssd.Create(ctx, nil, SubnetCreateInput{
		Name:                       "test1",
		Description:                cutil.GetPtr("test"),
		Org:                        "test",
		SiteID:                     site.ID,
		VpcID:                      vpc.ID,
		DomainID:                   &domain.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: &dummyUUID,
		RoutingType:                &ipv4Block.RoutingType,
		IPv4Prefix:                 &ipv4Prefix,
		IPv4Gateway:                &ipv4Gateway,
		IPv4BlockID:                &ipv4Block.ID,
		IPv6Prefix:                 &ipv6Prefix,
		IPv6Gateway:                &ipv6Gateway,
		IPv6BlockID:                &ipv6Block.ID,
		PrefixLength:               8,
		Status:                     SubnetStatusPending,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, subnet1)
	subnet2, err := ssd.Create(ctx, nil, SubnetCreateInput{
		Name:                       "test2",
		Description:                cutil.GetPtr("test"),
		Org:                        "test",
		SiteID:                     site.ID,
		VpcID:                      vpc.ID,
		DomainID:                   &domain.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: &dummyUUID,
		RoutingType:                &ipv4Block.RoutingType,
		IPv4Prefix:                 &ipv4Prefix,
		IPv4Gateway:                &ipv4Gateway,
		IPv4BlockID:                &ipv4Block.ID,
		IPv6Prefix:                 &ipv6Prefix,
		IPv6Gateway:                &ipv6Gateway,
		IPv6BlockID:                &ipv6Block.ID,
		PrefixLength:               8,
		Status:                     SubnetStatusPending,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, subnet2)

	vpsd := NewVpcPrefixDAO(dbSession)
	vpcPrefix1, err := vpsd.Create(ctx, nil, VpcPrefixCreateInput{
		Name:         "VpcPrefix-1",
		TenantOrg:    "test",
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipv4Block.ID,
		Prefix:       ipv4Prefix,
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vpcPrefix1)

	vpcPrefix2, err := vpsd.Create(ctx, nil, VpcPrefixCreateInput{
		Name:         "VpcPrefix-2",
		TenantOrg:    "test",
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipv4Block.ID,
		Prefix:       ipv4Prefix,
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    user.ID,
	})

	assert.Nil(t, err)
	assert.NotNil(t, vpcPrefix2)

	ifcd := NewInterfaceDAO(dbSession)

	input1 := InterfaceCreateInput{
		InstanceID: i1.ID,
		SubnetID:   &subnet1.ID,
		IsPhysical: false,
		Status:     InterfaceStatusPending,
		CreatedBy:  user.ID,
	}

	ifc, err := ifcd.Create(ctx, nil, input1)
	assert.Nil(t, err)
	assert.NotNil(t, ifc)

	vfID := 10
	macAddress := "21-41-A7-A6-40-76"
	ipAddresses := []string{"192.0.2.3", "2001:db8:abcd::18"}
	routingProfile := &InterfaceInlineRoutingProfile{
		AllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
	}

	input2 := InterfaceCreateInput{
		InstanceID:  i1.ID,
		VpcPrefixID: &vpcPrefix1.ID,
		IsPhysical:  true,
		Status:      InterfaceStatusPending,
		CreatedBy:   user.ID,
	}

	ifc1, err := ifcd.Create(ctx, nil, input2)
	assert.Nil(t, err)
	assert.NotNil(t, ifc1)

	ifcRouting, err := ifcd.Create(ctx, nil, input2)
	assert.Nil(t, err)
	assert.NotNil(t, ifcRouting)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                      string
		id                        uuid.UUID
		paramInstanceID           *uuid.UUID
		paramSubnetID             *uuid.UUID
		paramVpcID                *uuid.UUID
		paramVpcIPFamilyMode      *InterfaceVpcIPFamilyMode
		paramVpcPrefixID          *uuid.UUID
		paramSecondaryVpcPrefixID *uuid.UUID
		paramDevice               *string
		paramDeviceInstance       *int
		paramVirtualFunctionID    *int
		paramRequestedIpAddress   *string
		paramRoutingProfile       *InterfaceInlineRoutingProfile
		paramMacAddress           *string
		paramIPAddresses          []string
		paramStatus               *string

		expectedInstanceID           *uuid.UUID
		expectedSubnetID             *uuid.UUID
		expectedVpcID                *uuid.UUID
		expectedVpcIPFamilyMode      *InterfaceVpcIPFamilyMode
		expectedVpcPrefixID          *uuid.UUID
		expectedSecondaryVpcPrefixID *uuid.UUID
		expectedDevice               *string
		expectedDeviceInstance       *int
		expectedVirtualFunctionID    *int
		expectedRequestedIpAddress   *string
		expectedRoutingProfile       *InterfaceInlineRoutingProfile
		expectedMacAddress           *string
		expectedIPAddresses          []string
		expectedStatus               *string

		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:                    "success wth subnet fields updated",
			id:                      ifc.ID,
			paramInstanceID:         &i2.ID,
			paramSubnetID:           &subnet2.ID,
			paramVirtualFunctionID:  &vfID,
			paramRequestedIpAddress: cutil.GetPtr("192.0.2.21"),
			paramMacAddress:         &macAddress,
			paramIPAddresses:        ipAddresses,
			paramStatus:             cutil.GetPtr(InterfaceStatusReady),

			expectedInstanceID:         &i2.ID,
			expectedSubnetID:           &subnet2.ID,
			expectedVirtualFunctionID:  &vfID,
			expectedRequestedIpAddress: cutil.GetPtr("192.0.2.21"),
			expectedMacAddress:         &macAddress,
			expectedIPAddresses:        ipAddresses,
			expectedStatus:             cutil.GetPtr(InterfaceStatusReady),

			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:                      "success wth vpcprefix fields updated",
			id:                        ifcRouting.ID,
			paramInstanceID:           &i2.ID,
			paramVpcID:                &vpc.ID,
			paramVpcIPFamilyMode:      cutil.GetPtr(InterfaceVpcIPFamilyModeIPv4Only),
			paramVpcPrefixID:          &vpcPrefix2.ID,
			paramSecondaryVpcPrefixID: &vpcPrefix1.ID,
			paramVirtualFunctionID:    &vfID,
			paramRequestedIpAddress:   cutil.GetPtr("192.0.2.31"),
			paramRoutingProfile:       routingProfile,
			paramMacAddress:           &macAddress,
			paramIPAddresses:          ipAddresses,
			paramStatus:               cutil.GetPtr(InterfaceStatusReady),

			expectedInstanceID:           &i2.ID,
			expectedVpcID:                &vpc.ID,
			expectedVpcIPFamilyMode:      cutil.GetPtr(InterfaceVpcIPFamilyModeIPv4Only),
			expectedVpcPrefixID:          &vpcPrefix2.ID,
			expectedSecondaryVpcPrefixID: &vpcPrefix1.ID,
			expectedVirtualFunctionID:    &vfID,
			expectedRequestedIpAddress:   cutil.GetPtr("192.0.2.31"),
			expectedRoutingProfile:       routingProfile,
			expectedMacAddress:           &macAddress,
			expectedIPAddresses:          ipAddresses,
			expectedStatus:               cutil.GetPtr(InterfaceStatusReady),

			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:                    "success wth device fields updated",
			paramInstanceID:         &i2.ID,
			id:                      ifc1.ID,
			paramDevice:             cutil.GetPtr("MT43344 BlueField-3 integrated ConnectX-7 network controller"),
			paramDeviceInstance:     cutil.GetPtr(1),
			paramVirtualFunctionID:  &vfID,
			paramRequestedIpAddress: cutil.GetPtr("192.0.2.41"),
			paramMacAddress:         &macAddress,
			paramIPAddresses:        ipAddresses,
			paramStatus:             cutil.GetPtr(InterfaceStatusReady),

			expectedInstanceID:         &i2.ID,
			expectedDevice:             cutil.GetPtr("MT43344 BlueField-3 integrated ConnectX-7 network controller"),
			expectedDeviceInstance:     cutil.GetPtr(1),
			expectedVirtualFunctionID:  &vfID,
			expectedRequestedIpAddress: cutil.GetPtr("192.0.2.41"),
			expectedMacAddress:         &macAddress,
			expectedIPAddresses:        ipAddresses,
			expectedStatus:             cutil.GetPtr(InterfaceStatusReady),

			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:        "failed when id not found",
			id:          uuid.New(),
			paramStatus: cutil.GetPtr(InterfaceStatusProvisioning),
			expectError: true,
		},
		{
			desc:             "failed with malformed IP address",
			id:               ifc1.ID,
			paramIPAddresses: []string{"not-an-ip"},
			expectError:      true,
		},
		{
			desc:             "failed with CIDR-form IP address",
			id:               ifc1.ID,
			paramIPAddresses: []string{"192.0.2.1/31"},
			expectError:      true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			input := InterfaceUpdateInput{
				InterfaceID:          tc.id,
				InstanceID:           tc.paramInstanceID,
				SubnetID:             tc.paramSubnetID,
				VpcID:                tc.paramVpcID,
				VpcIPFamilyMode:      tc.paramVpcIPFamilyMode,
				VpcPrefixID:          tc.paramVpcPrefixID,
				SecondaryVpcPrefixID: tc.paramSecondaryVpcPrefixID,
				Device:               tc.paramDevice,
				DeviceInstance:       tc.paramDeviceInstance,
				VirtualFunctionID:    tc.paramVirtualFunctionID,
				RequestedIpAddress:   tc.paramRequestedIpAddress,
				InlineRoutingProfile: tc.paramRoutingProfile,
				MacAddress:           tc.paramMacAddress,
				IpAddresses:          tc.paramIPAddresses,
				Status:               tc.paramStatus,
			}
			got, err := ifcd.Update(ctx, nil, input)
			assert.Equal(t, tc.expectError, err != nil)
			if err == nil {
				assert.EqualValues(t, tc.id, got.ID)
				require.NoError(t, err)

				assert.Equal(t, *tc.expectedInstanceID, got.InstanceID)
				if tc.expectedSubnetID != nil {
					assert.Equal(t, *tc.expectedSubnetID, *got.SubnetID)
				}
				if tc.expectedVpcID != nil {
					assert.Equal(t, *tc.expectedVpcID, *got.VpcID)
				}
				if tc.expectedVpcIPFamilyMode != nil {
					assert.Equal(t, *tc.expectedVpcIPFamilyMode, *got.VpcIPFamilyMode)
				}
				if tc.expectedVpcPrefixID != nil {
					assert.Equal(t, *tc.expectedVpcPrefixID, *got.VpcPrefixID)
				}
				assert.Equal(t, tc.expectedSecondaryVpcPrefixID, got.SecondaryVpcPrefixID)
				assert.Equal(t, *tc.expectedVirtualFunctionID, *got.VirtualFunctionID)
				assert.Equal(t, tc.expectedRequestedIpAddress, got.RequestedIpAddress)
				assert.Equal(t, tc.expectedRoutingProfile, got.InlineRoutingProfile)
				assert.Equal(t, *tc.expectedMacAddress, *got.MacAddress)
				assert.Equal(t, tc.expectedIPAddresses, got.IPAddresses)
				assert.Equal(t, *tc.expectedStatus, got.Status)

				persisted, err := ifcd.GetByID(ctx, nil, tc.id, nil)
				require.NoError(t, err)
				assert.Equal(t, tc.expectedRoutingProfile, persisted.InlineRoutingProfile)
				assert.Equal(t, tc.expectedVpcID, persisted.VpcID)
				assert.Equal(t, tc.expectedVpcIPFamilyMode, persisted.VpcIPFamilyMode)

				if tc.expectedDevice != nil {
					assert.Equal(t, *tc.expectedDevice, *got.Device)
				}
				if tc.expectedDeviceInstance != nil {
					assert.Equal(t, *tc.expectedDeviceInstance, *got.DeviceInstance)
				}

				if got.Updated.String() == ifc.Updated.String() {
					t.Errorf("got.Updated = %v, want different value", got.Updated)
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

func TestInterfaceSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInterfaceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType.ID), cutil.GetPtr("mcTypeTest"))
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
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
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
	assert.NotNil(t, i1)
	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", cutil.GetPtr(user.ID))
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", cutil.GetPtr(user.ID))

	ssd := NewSubnetDAO(dbSession)
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	subnet, err := ssd.Create(ctx, nil, SubnetCreateInput{
		Name:                       "test",
		Description:                cutil.GetPtr("test"),
		Org:                        "test",
		SiteID:                     site.ID,
		VpcID:                      vpc.ID,
		DomainID:                   &domain.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: &dummyUUID,
		RoutingType:                &ipv4Block.RoutingType,
		IPv4Prefix:                 &ipv4Prefix,
		IPv4Gateway:                &ipv4Gateway,
		IPv4BlockID:                &ipv4Block.ID,
		IPv6Prefix:                 &ipv6Prefix,
		IPv6Gateway:                &ipv6Gateway,
		IPv6BlockID:                &ipv6Block.ID,
		PrefixLength:               8,
		Status:                     SubnetStatusPending,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, subnet)

	ifcd := NewInterfaceDAO(dbSession)

	input := InterfaceCreateInput{
		InstanceID: i1.ID,
		SubnetID:   &subnet.ID,
		IsPhysical: true,
		Status:     InterfaceStatusPending,
		CreatedBy:  user.ID,
	}

	ifc, err := ifcd.Create(ctx, nil, input)
	assert.Nil(t, err)
	assert.NotNil(t, ifc)

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
			id:                 ifc.ID,
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
			err := ifcd.Delete(ctx, nil, tc.id)
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

func TestInterfaceSQLDAO_CreateMultiple(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInterfaceSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType.ID), cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	isd := NewInstanceDAO(dbSession)
	instance1, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test-instance-1",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test1.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.Nil(t, err)
	machine2 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType.ID), cutil.GetPtr("mcTypeTest2"))
	instance2, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test-instance-2",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine2.ID,
			Hostname:                 cutil.GetPtr("test2.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.Nil(t, err)

	ifcd := NewInterfaceDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []InterfaceCreateInput
		expectError        bool
		expectedCount      int
		verifyChildSpanner bool
	}{
		{
			desc: "create batch of three interfaces",
			inputs: []InterfaceCreateInput{
				{
					InstanceID: instance1.ID,
					IsPhysical: true,
					InlineRoutingProfile: &InterfaceInlineRoutingProfile{
						AllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
					},
					Status:    InterfaceStatusPending,
					CreatedBy: user.ID,
				},
				{
					InstanceID: instance1.ID,
					IsPhysical: false,
					Status:     InterfaceStatusReady,
					CreatedBy:  user.ID,
				},
				{
					InstanceID: instance2.ID,
					IsPhysical: true,
					Status:     InterfaceStatusPending,
					CreatedBy:  user.ID,
				},
			},
			expectError:        false,
			expectedCount:      3,
			verifyChildSpanner: true,
		},
		{
			desc:               "create batch with empty input",
			inputs:             []InterfaceCreateInput{},
			expectError:        false,
			expectedCount:      0,
			verifyChildSpanner: false,
		},
		{
			desc: "create batch with single interface",
			inputs: []InterfaceCreateInput{
				{
					InstanceID: instance1.ID,
					IsPhysical: true,
					Status:     InterfaceStatusPending,
					CreatedBy:  user.ID,
				},
			},
			expectError:   false,
			expectedCount: 1,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := ifcd.CreateMultiple(ctx, nil, tc.inputs)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Verify each created interface has a valid ID
				// Also verify that results are returned in the same order as inputs
				for i, iface := range got {
					assert.NotEqual(t, uuid.Nil, iface.ID)
					assert.Equal(t, tc.inputs[i].InstanceID, iface.InstanceID, "result order should match input order")
					assert.Equal(t, tc.inputs[i].InlineRoutingProfile, iface.InlineRoutingProfile)
					assert.Equal(t, tc.inputs[i].Status, iface.Status)
					assert.NotZero(t, iface.Created)
					assert.NotZero(t, iface.Updated)
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

func TestInterfaceSQLDAO_CreateMultiple_ExceedsMaxBatchItems(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	ifcd := NewInterfaceDAO(dbSession)

	// Create inputs exceeding MaxBatchItems
	inputs := make([]InterfaceCreateInput, db.MaxBatchItems+1)
	for i := range inputs {
		inputs[i] = InterfaceCreateInput{
			InstanceID: uuid.New(),
			IsPhysical: true,
			Status:     InterfaceStatusPending,
		}
	}

	_, err := ifcd.CreateMultiple(ctx, nil, inputs)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "batch size")
	assert.Contains(t, err.Error(), "exceeds maximum allowed")
}

func TestInterfaceSQLDAO_DeleteAllByInstanceIDs(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testInterfaceSetupSchema(t, dbSession)

	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	isd := NewInstanceDAO(dbSession)

	buildInstance := func(name, hostname, machineTag string) *Instance {
		machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType.ID), cutil.GetPtr(machineTag))
		instance, err := isd.Create(
			ctx, nil,
			InstanceCreateInput{
				Name:                     name,
				TenantID:                 tenant.ID,
				InfrastructureProviderID: ip.ID,
				SiteID:                   site.ID,
				InstanceTypeID:           &instanceType.ID,
				VpcID:                    vpc.ID,
				MachineID:                &machine.ID,
				Hostname:                 cutil.GetPtr(hostname),
				OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
				IpxeScript:               cutil.GetPtr("ipxe"),
				UserData:                 cutil.GetPtr("userdata"),
				Labels:                   map[string]string{},
				Status:                   InstanceStatusPending,
				CreatedBy:                user.ID,
			},
		)
		require.NoError(t, err)
		return instance
	}

	// Three instances: instance1 and instance2 are deletion targets, instance3 is the
	// "untouched" control to confirm the IN-clause is properly scoped.
	instance1 := buildInstance("test-instance-1", "test1.com", "mcType1")
	instance2 := buildInstance("test-instance-2", "test2.com", "mcType2")
	instance3 := buildInstance("test-instance-3", "test3.com", "mcType3")

	ifcd := NewInterfaceDAO(dbSession)

	makeIfaceInput := func(instanceID uuid.UUID) InterfaceCreateInput {
		return InterfaceCreateInput{
			InstanceID: instanceID,
			IsPhysical: true,
			Status:     InterfaceStatusPending,
			CreatedBy:  user.ID,
		}
	}

	ifc1a, err := ifcd.Create(ctx, nil, makeIfaceInput(instance1.ID))
	require.NoError(t, err)
	require.NotNil(t, ifc1a)
	ifc1b, err := ifcd.Create(ctx, nil, makeIfaceInput(instance1.ID))
	require.NoError(t, err)
	require.NotNil(t, ifc1b)
	ifc2, err := ifcd.Create(ctx, nil, makeIfaceInput(instance2.ID))
	require.NoError(t, err)
	require.NotNil(t, ifc2)
	ifc3, err := ifcd.Create(ctx, nil, makeIfaceInput(instance3.ID))
	require.NoError(t, err)
	require.NotNil(t, ifc3)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	// Empty input should be a no-op (and must not produce invalid SQL).
	err = ifcd.DeleteAllByInstanceIDs(ctx, nil, nil)
	require.NoError(t, err)
	err = ifcd.DeleteAllByInstanceIDs(ctx, nil, []uuid.UUID{})
	require.NoError(t, err)

	// All four interfaces should still be present.
	for _, id := range []uuid.UUID{ifc1a.ID, ifc1b.ID, ifc2.ID, ifc3.ID} {
		row := &Interface{}
		serr := dbSession.DB.NewSelect().Model(row).Where("id = ?", id).Scan(context.Background())
		require.NoError(t, serr, "expected interface %s to still be present after no-op delete", id)
		assert.Nil(t, row.Deleted)
	}

	// Bulk-delete interfaces for instance1 and instance2.
	err = ifcd.DeleteAllByInstanceIDs(ctx, nil, []uuid.UUID{instance1.ID, instance2.ID})
	require.NoError(t, err)

	// Targeted interfaces should be soft-deleted.
	for _, id := range []uuid.UUID{ifc1a.ID, ifc1b.ID, ifc2.ID} {
		deleted := &Interface{}
		serr := dbSession.DB.NewSelect().Model(deleted).WhereDeleted().Where("id = ?", id).Scan(context.Background())
		require.NoError(t, serr, "expected soft-deleted row for id %s", id)
		assert.NotNil(t, deleted.Deleted)

		// Default selects (which exclude soft-deleted rows) should not return them.
		notFound := &Interface{}
		serr = dbSession.DB.NewSelect().Model(notFound).Where("id = ?", id).Scan(context.Background())
		assert.Error(t, serr, "soft-deleted row for id %s should not appear in default selects", id)
	}

	// instance3's interface must remain untouched.
	other := &Interface{}
	err = dbSession.DB.NewSelect().Model(other).Where("id = ?", ifc3.ID).Scan(context.Background())
	require.NoError(t, err)
	assert.Nil(t, other.Deleted)

	// Deleting against a wholly-unrelated instance ID set should also succeed without error.
	err = ifcd.DeleteAllByInstanceIDs(ctx, nil, []uuid.UUID{uuid.New()})
	require.NoError(t, err)

	// Verify the active span is propagated through the call.
	span := otrace.SpanFromContext(ctx)
	assert.True(t, span.SpanContext().IsValid())
	_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
	assert.True(t, ok)
}
