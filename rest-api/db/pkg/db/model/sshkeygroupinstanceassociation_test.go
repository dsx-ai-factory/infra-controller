// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	otrace "go.opentelemetry.io/otel/trace"
)

// reset the tables needed for SSHKeyGroupInstanceAssociation tests
func testSSHKeyGroupInstanceAssociationSetupSchema(t *testing.T, dbSession *db.Session) {
	testSSHKeyGroupSetupSchema(t, dbSession)
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
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*Vpc)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*IPBlock)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*Machine)(nil))
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
	// create the SSHKeyGroupInstanceAssociation table
	err = dbSession.DB.ResetModel(context.Background(), (*SSHKeyGroupInstanceAssociation)(nil))
	assert.Nil(t, err)
}

func TestSSHKeyGroupInstanceAssociationSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupInstanceAssociationSetupSchema(t, dbSession)
	user := testInstanceBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	sshKeyGroup1 := testBuildSSHKeyGroup(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)
	sshKeyGroup2 := testBuildSSHKeyGroup(t, dbSession, "test2", cutil.GetPtr("test2"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)
	isd := NewInstanceDAO(dbSession)
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
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i1)

	skgisd := NewSSHKeyGroupInstanceAssociationDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		skgias             []SSHKeyGroupInstanceAssociation
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			skgias: []SSHKeyGroupInstanceAssociation{
				{
					SSHKeyGroupID: sshKeyGroup1.ID, SiteID: site.ID, InstanceID: i1.ID, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple",
			skgias: []SSHKeyGroupInstanceAssociation{
				{
					SSHKeyGroupID: sshKeyGroup1.ID, SiteID: site.ID, InstanceID: i1.ID, CreatedBy: user.ID,
				},
				{
					SSHKeyGroupID: sshKeyGroup2.ID, SiteID: site.ID, InstanceID: i1.ID, CreatedBy: user.ID,
				},
			},
			expectError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, skgia := range tc.skgias {
				dbskgia, err := skgisd.Create(ctx, nil, SSHKeyGroupInstanceAssociationCreateInput{
					SSHKeyGroupID: skgia.SSHKeyGroupID,
					SiteID:        skgia.SiteID,
					InstanceID:    skgia.InstanceID,
					CreatedBy:     skgia.CreatedBy,
				})
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, dbskgia)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestSSHKeyGroupInstanceAssociationSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupInstanceAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	sshKeyGroup1 := testBuildSSHKeyGroup(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)
	isd := NewInstanceDAO(dbSession)
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
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)

	skaisd := NewSSHKeyGroupInstanceAssociationDAO(dbSession)
	skgis1, err := skaisd.Create(ctx, nil, SSHKeyGroupInstanceAssociationCreateInput{
		SSHKeyGroupID: sshKeyGroup1.ID,
		SiteID:        site.ID,
		InstanceID:    i1.ID,
		CreatedBy:     user.ID,
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                    string
		skgiaID                 uuid.UUID
		includeRelations        []string
		expectNotNilSSHKeyGroup bool
		expectNotNilSite        bool
		expectNotNilInstance    bool
		expectError             bool
		verifyChildSpanner      bool
	}{
		{
			desc:               "success without relations",
			skgiaID:            skgis1.ID,
			includeRelations:   []string{},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:                    "success with relations",
			skgiaID:                 skgis1.ID,
			includeRelations:        []string{SSHKeyGroupRelationName, SiteRelationName, InstanceRelationName},
			expectError:             false,
			expectNotNilSSHKeyGroup: true,
		},
		{
			desc:             "error when not found",
			skgiaID:          uuid.New(),
			includeRelations: []string{},
			expectError:      true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := skaisd.GetByID(ctx, nil, tc.skgiaID, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				if tc.expectNotNilSSHKeyGroup {
					assert.NotNil(t, got.SSHKeyGroup)
				}
				if tc.expectNotNilSite {
					assert.NotNil(t, got.Site)
				}
				if tc.expectNotNilInstance {
					assert.NotNil(t, got.Instance)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupInstanceAssociationSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupInstanceAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")

	skgs := []*SSHKeyGroup{}
	skgias := []*SSHKeyGroupInstanceAssociation{}
	skgsd := NewSSHKeyGroupDAO(dbSession)
	isd := NewInstanceDAO(dbSession)
	skgiasd := NewSSHKeyGroupInstanceAssociationDAO(dbSession)
	for i := 1; i <= 25; i++ {
		skg1, err := skgsd.Create(
			ctx,
			nil,
			SSHKeyGroupCreateInput{
				Name:        fmt.Sprintf("test-%d", i),
				Description: cutil.GetPtr(fmt.Sprintf("test-%d", i)),
				TenantOrg:   "testorg",
				TenantID:    tenant.ID,
				Status:      SSHKeyGroupStatusSyncing,
				CreatedBy:   user.ID,
			},
		)
		skgs = append(skgs, skg1)
		assert.Nil(t, err)
		assert.NotNil(t, skg1)

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
				InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
				Status:                   InstanceStatusPending,
				CreatedBy:                user.ID,
			},
		)

		assert.Nil(t, err)
		assert.NotNil(t, i1)

		skgia1, err := skgiasd.Create(ctx, nil, SSHKeyGroupInstanceAssociationCreateInput{
			SSHKeyGroupID: skg1.ID,
			SiteID:        site.ID,
			InstanceID:    i1.ID,
			CreatedBy:     user.ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, skgia1)
		skgias = append(skgias, skgia1)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc             string
		includeRelations []string

		paramSSHKeyGroupIDs []uuid.UUID
		paramSiteIDs        []uuid.UUID
		paramInstanceIDs    []uuid.UUID

		paramOffset  *int
		paramLimit   *int
		paramOrderBy *paginator.OrderBy

		expectCnt                                         int
		expectTotal                                       int
		expectFirstObjectSSHKeyGroupInstanceAssociationID string

		expectNotNilSSHKeyGroup bool
		expectNotNilSite        bool
		expectNotNilInstance    bool
		expectError             bool
		verifyChildSpanner      bool
	}{
		{
			desc:                "getall with SSHKeyGroup filters but no relations returns objects",
			paramSSHKeyGroupIDs: []uuid.UUID{skgs[0].ID, skgs[1].ID},
			includeRelations:    []string{},
			expectFirstObjectSSHKeyGroupInstanceAssociationID: skgias[0].ID.String(),
			expectError:        false,
			expectTotal:        2,
			expectCnt:          2,
			verifyChildSpanner: true,
		},
		{
			desc:             "getall with filters and relations returns objects",
			includeRelations: []string{SSHKeyGroupRelationName},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectSSHKeyGroupInstanceAssociationID: skgias[0].ID.String(),
			expectError:             false,
			expectTotal:             25,
			expectCnt:               20,
			expectNotNilSSHKeyGroup: true,
		},
		{
			desc:             "getall with site filters and relations returns objects",
			includeRelations: []string{SiteRelationName},
			paramSiteIDs:     []uuid.UUID{site.ID},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectSSHKeyGroupInstanceAssociationID: skgias[0].ID.String(),
			expectError:      false,
			expectTotal:      25,
			expectCnt:        20,
			expectNotNilSite: true,
		},
		{
			desc:             "getall with instance filters and relations returns objects",
			includeRelations: []string{InstanceRelationName},
			paramInstanceIDs: []uuid.UUID{skgias[0].InstanceID},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectSSHKeyGroupInstanceAssociationID: skgias[0].ID.String(),
			expectError:          false,
			expectTotal:          1,
			expectCnt:            1,
			expectNotNilInstance: true,
		},
		{
			desc:             "getall with offset, limit returns objects",
			includeRelations: []string{},
			paramOffset:      cutil.GetPtr(10),
			paramLimit:       cutil.GetPtr(10),
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectSSHKeyGroupInstanceAssociationID: skgias[10].ID.String(),
			expectError: false,
			expectTotal: 25,
			expectCnt:   10,
		},
		{
			desc:                "case when no objects are returned",
			includeRelations:    []string{},
			expectError:         false,
			paramSSHKeyGroupIDs: []uuid.UUID{uuid.New()},
			expectTotal:         0,
			expectCnt:           0,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			objs, tot, err := skgiasd.GetAll(
				ctx,
				nil,
				SSHKeyGroupInstanceAssociationFilterInput{
					SSHKeyGroupIDs: tc.paramSSHKeyGroupIDs,
					SiteIDs:        tc.paramSiteIDs,
					InstanceIDs:    tc.paramInstanceIDs,
				},
				paginator.PageInput{
					Offset:  tc.paramOffset,
					Limit:   tc.paramLimit,
					OrderBy: tc.paramOrderBy,
				},
				tc.includeRelations,
			)
			assert.Equal(t, tc.expectError, err != nil)
			assert.Equal(t, tc.expectCnt, len(objs))
			assert.Equal(t, tc.expectTotal, tot)
			if len(objs) > 0 {
				if tc.expectFirstObjectSSHKeyGroupInstanceAssociationID != "" {
					assert.Equal(t, tc.expectFirstObjectSSHKeyGroupInstanceAssociationID, objs[0].ID.String())
				}

				if tc.expectNotNilSSHKeyGroup {
					assert.NotNil(t, objs[0].SSHKeyGroup)
				}

				if tc.expectNotNilSite {
					assert.NotNil(t, objs[0].Site)
				}

				if tc.expectNotNilInstance {
					assert.NotNil(t, objs[0].Instance)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupInstanceAssociationSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupInstanceAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	site2 := TestBuildSite(t, dbSession, ip, "test2", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")

	skgiasd := NewSSHKeyGroupInstanceAssociationDAO(dbSession)
	sshKeyGroup1 := testBuildSSHKeyGroup(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)
	sshKeyGroup2 := testBuildSSHKeyGroup(t, dbSession, "test2", cutil.GetPtr("test2"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)

	isd := NewInstanceDAO(dbSession)
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
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)

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
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)

	skgisa1, err := skgiasd.Create(ctx, nil, SSHKeyGroupInstanceAssociationCreateInput{
		SSHKeyGroupID: sshKeyGroup1.ID,
		SiteID:        site.ID,
		InstanceID:    i1.ID,
		CreatedBy:     user.ID,
	})
	assert.Nil(t, err)

	skgisa2, err := skgiasd.Create(ctx, nil, SSHKeyGroupInstanceAssociationCreateInput{
		SSHKeyGroupID: sshKeyGroup2.ID,
		SiteID:        site.ID,
		InstanceID:    i2.ID,
		CreatedBy:     user.ID,
	})
	assert.NotNil(t, skgisa2)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc  string
		input SSHKeyGroupInstanceAssociationUpdateInput

		expectedSSHKeyGroupID *uuid.UUID
		expectedSiteID        *uuid.UUID
		expectedInstanceID    *uuid.UUID

		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "can update all fields",
			input: SSHKeyGroupInstanceAssociationUpdateInput{
				SSHKeyGroupInstanceAssociationID: skgisa1.ID,
				SSHKeyGroupID:                    cutil.GetPtr(sshKeyGroup2.ID),
				SiteID:                           cutil.GetPtr(site2.ID),
				InstanceID:                       cutil.GetPtr(i2.ID),
			},

			expectedSSHKeyGroupID: cutil.GetPtr(sshKeyGroup2.ID),
			expectedSiteID:        cutil.GetPtr(site2.ID),
			expectedInstanceID:    cutil.GetPtr(i2.ID),

			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:        "error when ID not found",
			input:       SSHKeyGroupInstanceAssociationUpdateInput{SSHKeyGroupInstanceAssociationID: uuid.New()},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := skgiasd.Update(ctx, nil, tc.input)
			assert.Equal(t, tc.expectError, err != nil)
			if err == nil {
				assert.Equal(t, tc.expectedSSHKeyGroupID.String(), got.SSHKeyGroupID.String())
				assert.Equal(t, tc.expectedSiteID.String(), got.SiteID.String())
				assert.Equal(t, tc.expectedInstanceID.String(), got.InstanceID.String())
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupInstanceAssociationSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyGroupInstanceAssociationSetupSchema(t, dbSession)
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ip := testBuildInfrastructureProvider(t, dbSession, cutil.GetPtr(uuid.New()), "test", "testorg", user.ID)
	site := TestBuildSite(t, dbSession, ip, "test", user)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")

	skgiasd := NewSSHKeyGroupInstanceAssociationDAO(dbSession)
	sshKeyGroup1 := testBuildSSHKeyGroup(t, dbSession, "test1", cutil.GetPtr("test1"), "tesorg", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)

	isd := NewInstanceDAO(dbSession)
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
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                user.ID,
		},
	)

	skgias1, err := skgiasd.Create(ctx, nil, SSHKeyGroupInstanceAssociationCreateInput{
		SSHKeyGroupID: sshKeyGroup1.ID,
		SiteID:        site.ID,
		InstanceID:    i1.ID,
		CreatedBy:     user.ID,
	})
	assert.Nil(t, err)

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
			id:                 skgias1.ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "delete non-existing object",
			id:            uuid.New(),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := skgiasd.Delete(ctx, nil, tc.id)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := skgiasd.GetByID(ctx, nil, tc.id, nil)
				assert.NotNil(t, err)
				assert.Nil(t, tmp)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupInstanceAssociationSQLDAO_CreateMultiple(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	TestSetupSchema(t, dbSession)

	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoetenant@test.com"), cutil.GetPtr("Tenant"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	vpc := testInstanceBuildVpc(t, dbSession, ip, st, tn, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tn, st, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")
	isd := NewInstanceDAO(dbSession)
	instance1, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test1",
			TenantID:                 tn.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   st.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			UserData:                 cutil.GetPtr("userdata"),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	machine2 := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest2"))
	instance2, err := isd.Create(
		ctx, nil,
		InstanceCreateInput{
			Name:                     "test2",
			TenantID:                 tn.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   st.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine2.ID,
			Hostname:                 cutil.GetPtr("test2.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			UserData:                 cutil.GetPtr("userdata"),
			InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
			Status:                   InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	keyGroup := testBuildSSHKeyGroup(t, dbSession, "test-keygroup", cutil.GetPtr("Test SSH Key Group"), tn.Org, tn.ID, nil, SSHKeyGroupStatusSynced, tnu.ID)

	skgiasd := NewSSHKeyGroupInstanceAssociationDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []SSHKeyGroupInstanceAssociationCreateInput
		expectError        bool
		expectedCount      int
		verifyChildSpanner bool
	}{
		{
			desc: "create batch of three associations",
			inputs: []SSHKeyGroupInstanceAssociationCreateInput{
				{
					SSHKeyGroupID: keyGroup.ID,
					SiteID:        st.ID,
					InstanceID:    instance1.ID,
					CreatedBy:     tnu.ID,
				},
				{
					SSHKeyGroupID: keyGroup.ID,
					SiteID:        st.ID,
					InstanceID:    instance2.ID,
					CreatedBy:     tnu.ID,
				},
				{
					SSHKeyGroupID: keyGroup.ID,
					SiteID:        st.ID,
					InstanceID:    instance1.ID,
					CreatedBy:     tnu.ID,
				},
			},
			expectError:        false,
			expectedCount:      3,
			verifyChildSpanner: true,
		},
		{
			desc:               "create batch with empty input",
			inputs:             []SSHKeyGroupInstanceAssociationCreateInput{},
			expectError:        false,
			expectedCount:      0,
			verifyChildSpanner: false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := skgiasd.CreateMultiple(ctx, nil, tc.inputs)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Verify results are returned in the same order as inputs
				for i, skgia := range got {
					assert.NotEqual(t, uuid.Nil, skgia.ID)
					assert.Equal(t, tc.inputs[i].SSHKeyGroupID, skgia.SSHKeyGroupID, "result order should match input order")
					assert.Equal(t, tc.inputs[i].InstanceID, skgia.InstanceID)
					assert.NotZero(t, skgia.Created)
				}
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyGroupInstanceAssociationSQLDAO_CreateMultiple_ExceedsMaxBatchItems(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	skgiasd := NewSSHKeyGroupInstanceAssociationDAO(dbSession)

	// Create inputs exceeding MaxBatchItems
	inputs := make([]SSHKeyGroupInstanceAssociationCreateInput, db.MaxBatchItems+1)
	for i := range inputs {
		inputs[i] = SSHKeyGroupInstanceAssociationCreateInput{
			SSHKeyGroupID: uuid.New(),
			SiteID:        uuid.New(),
			InstanceID:    uuid.New(),
			CreatedBy:     uuid.New(),
		}
	}

	_, err := skgiasd.CreateMultiple(ctx, nil, inputs)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "batch size")
	assert.Contains(t, err.Error(), "exceeds maximum allowed")
}
