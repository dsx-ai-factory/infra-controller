// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"fmt"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
)

// reset the tables needed for MachineInterface tests
func testMachineInterfaceSetupSchema(t *testing.T, dbSession *db.Session) {
	testMachineSetupSchema(t, dbSession)
	// create subnet
	err := dbSession.DB.ResetModel(context.Background(), (*Subnet)(nil))
	assert.Nil(t, err)

	// create MachineInterface
	err = dbSession.DB.ResetModel(context.Background(), (*MachineInterface)(nil))
	assert.Nil(t, err)
}

func TestMachineInterfaceSQLDAO_Create(t *testing.T) {
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testMachineInterfaceSetupSchema(t, dbSession)

	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVPC")
	subnet := testInstanceBuildSubnet(t, dbSession, tenant, vpc, "testsubnet")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("mcTypeTest"))
	miDAO := NewMachineInterfaceDAO(dbSession)

	badSession, err := db.NewSession(context.Background(), "localhost", 1234, "postgres", "postgres", "postgres", "")
	assert.Nil(t, err)
	badDAO := NewMachineInterfaceDAO(badSession)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		desc               string
		dao                MachineInterfaceDAO
		mis                []MachineInterface
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			dao:  miDAO,
			mis: []MachineInterface{
				{
					MachineID: machine.ID, Hostname: cutil.GetPtr("test.com"), IsPrimary: true, AttachedDPUMachineID: cutil.GetPtr(uuid.New().String()), SubnetID: cutil.GetPtr(subnet.ID),
					MacAddress: cutil.GetPtr("00:00:00:00:00:00"), IPAddresses: []string{"192.168.0.1, 172.168.0.1"},
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "error failure due to bad session",
			dao:  badDAO,
			mis: []MachineInterface{
				{
					MachineID: machine.ID, Hostname: cutil.GetPtr("test.com"), IsPrimary: true, SubnetID: cutil.GetPtr(subnet.ID),
					MacAddress: cutil.GetPtr("00:00:00:00:00:00"), IPAddresses: []string{"192.168.0.1, 172.168.0.1"},
				},
			},
			expectError: true,
		},
		{
			desc: "error create fail due to foreign key violation on machine",
			dao:  miDAO,
			mis: []MachineInterface{
				{
					MachineID: uuid.NewString(), Hostname: cutil.GetPtr("test.com"), IsPrimary: true, SubnetID: cutil.GetPtr(subnet.ID),
					MacAddress: cutil.GetPtr("00:00:00:00:00:00"), IPAddresses: []string{"192.168.0.1, 172.168.0.1"},
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.mis {
				it, err := tc.dao.Create(
					ctx, nil, MachineInterfaceCreateInput{MachineID: i.MachineID, ControllerInterfaceID: i.ControllerInterfaceID, ControllerSegmentID: i.ControllerSegmentID, AttachedDpuMachineID: i.AttachedDPUMachineID, SubnetID: i.SubnetID,
						Hostname: i.Hostname, IsPrimary: i.IsPrimary, MacAddress: i.MacAddress, IpAddresses: i.IPAddresses,
					})
				assert.Equal(t, tc.expectError, err != nil)
				if err != nil {
					fmt.Println(err)
				}
				if !tc.expectError {
					assert.NotNil(t, it)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestMachineInterfaceSQLDAO_GetByID(t *testing.T) {
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testMachineInterfaceSetupSchema(t, dbSession)

	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVPC")
	subnet := testInstanceBuildSubnet(t, dbSession, tenant, vpc, "testsubnet")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("mcTypeTest"))
	miDAO := NewMachineInterfaceDAO(dbSession)
	ctx := context.Background()
	mi, err := miDAO.Create(
		ctx, nil, MachineInterfaceCreateInput{MachineID: machine.ID, ControllerInterfaceID: cutil.GetPtr(uuid.New()), ControllerSegmentID: cutil.GetPtr(uuid.New()), AttachedDpuMachineID: cutil.GetPtr(uuid.New().String()), SubnetID: cutil.GetPtr(subnet.ID),
			Hostname: cutil.GetPtr("hostname"), IsPrimary: true, MacAddress: cutil.GetPtr("0:0:0:0:0:0"), IpAddresses: []string{"192.168.0.1, 172.168.0.1"},
		})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 uuid.UUID
		paramRelations     []string
		expectedError      bool
		expectedErrVal     error
		expectedMachine    bool
		expectedSubnet     bool
		verifyChildSpanner bool
	}{
		{
			desc:               "GetById success when exists",
			id:                 mi.ID,
			paramRelations:     []string{},
			expectedError:      false,
			expectedMachine:    false,
			expectedSubnet:     false,
			verifyChildSpanner: true,
		},
		{
			desc:            "GetById error when not found",
			id:              uuid.New(),
			paramRelations:  []string{},
			expectedError:   true,
			expectedErrVal:  db.ErrDoesNotExist,
			expectedMachine: false,
			expectedSubnet:  false,
		},
		{
			desc:            "GetById with the relations",
			id:              mi.ID,
			paramRelations:  []string{MachineRelationName, SubnetRelationName},
			expectedError:   false,
			expectedMachine: true,
			expectedSubnet:  true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := miDAO.GetByID(ctx, nil, tc.id, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.EqualValues(t, mi.ID, got.ID)
				assert.Equal(t, tc.expectedMachine, got.Machine != nil)
				if tc.expectedMachine {
					assert.EqualValues(t, machine.ID, got.Machine.ID)
				}
				assert.Equal(t, tc.expectedSubnet, got.Subnet != nil)
				if tc.expectedSubnet {
					assert.EqualValues(t, subnet.ID, got.Subnet.ID)
				}
				for i, v := range got.IPAddresses {
					assert.Equal(t, mi.IPAddresses[i], v)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestMachineInterfaceSQLDAO_GetAll(t *testing.T) {
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testMachineInterfaceSetupSchema(t, dbSession)

	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVPC")
	subnet := testInstanceBuildSubnet(t, dbSession, tenant, vpc, "testsubnet")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("mcTypeTest"))
	miDAO := NewMachineInterfaceDAO(dbSession)

	ctx := context.Background()

	controllerSegmentID := uuid.New()

	totalCount := 30

	mis := []MachineInterface{}

	for i := 0; i < totalCount; i++ {
		mi, err := miDAO.Create(
			ctx, nil, MachineInterfaceCreateInput{MachineID: machine.ID, ControllerInterfaceID: cutil.GetPtr(uuid.New()), ControllerSegmentID: &controllerSegmentID, AttachedDpuMachineID: cutil.GetPtr(uuid.New().String()), SubnetID: cutil.GetPtr(subnet.ID),
				Hostname: cutil.GetPtr(fmt.Sprintf("hostname-%v", i)), IsPrimary: true, MacAddress: cutil.GetPtr(testGenerateMacAddress(t)), IpAddresses: []string{fmt.Sprintf("192.168.0.%v", i), fmt.Sprintf("172.168.0.%v", i)},
			})
		assert.Nil(t, err)
		mis = append(mis, *mi)
	}

	badSession, err := db.NewSession(context.Background(), "localhost", 1234, "postgres", "postgres", "postgres", "")
	assert.Nil(t, err)
	badDAO := NewMachineInterfaceDAO(badSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                   string
		dao                    MachineInterfaceDAO
		machineIDs             []string
		controllerInterfaceIDs []uuid.UUID
		controllerSegmentIDs   []uuid.UUID
		AttachedDPUMachineIDs  []string
		subnetIDs              []uuid.UUID
		hostnames              []string
		offset                 *int
		limit                  *int
		orderBy                *paginator.OrderBy
		firstEntry             *MachineInterface
		expectedCount          int
		expectedTotal          *int
		expectedError          bool
		paramRelations         []string
		verifyChildSpanner     bool
	}{
		{
			desc:               "GetAll with no filters and bad DAO returns error",
			dao:                badDAO,
			expectedError:      true,
			verifyChildSpanner: true,
		},
		{
			desc:          "GetAll with no filters returns objects",
			dao:           miDAO,
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc:                   "GetAll with filters returns objects",
			dao:                    miDAO,
			machineIDs:             []string{machine.ID},
			controllerInterfaceIDs: []uuid.UUID{*mis[0].ControllerInterfaceID},
			controllerSegmentIDs:   []uuid.UUID{controllerSegmentID},
			subnetIDs:              []uuid.UUID{subnet.ID},
			hostnames:              []string{*mis[0].Hostname},
			expectedCount:          1,
			expectedError:          false,
		},
		{
			desc:                   "GetAll with relation returns objects",
			dao:                    miDAO,
			machineIDs:             []string{machine.ID},
			controllerInterfaceIDs: []uuid.UUID{*mis[0].ControllerInterfaceID},
			controllerSegmentIDs:   []uuid.UUID{controllerSegmentID},
			AttachedDPUMachineIDs:  []string{*mis[0].AttachedDPUMachineID},
			subnetIDs:              []uuid.UUID{subnet.ID},
			hostnames:              []string{*mis[0].Hostname},
			expectedCount:          1,
			expectedError:          false,
			paramRelations:         []string{MachineRelationName, SubnetRelationName},
		},
		{
			desc:                   "GetAll with filters returns no objects",
			dao:                    miDAO,
			machineIDs:             []string{machine.ID},
			controllerInterfaceIDs: []uuid.UUID{*mis[0].ControllerInterfaceID},
			controllerSegmentIDs:   []uuid.UUID{controllerSegmentID},
			subnetIDs:              []uuid.UUID{subnet.ID},
			hostnames:              []string{*mis[1].Hostname},
			expectedCount:          0,
			expectedError:          false,
		},
		{
			desc:          "GetAll with limit returns objects",
			dao:           miDAO,
			machineIDs:    []string{machine.ID},
			offset:        cutil.GetPtr(0),
			limit:         cutil.GetPtr(5),
			expectedCount: 5,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with offset returns objects",
			dao:           miDAO,
			machineIDs:    []string{machine.ID},
			offset:        cutil.GetPtr(15),
			expectedCount: 15,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:       "GetAll with order by returns objects",
			dao:        miDAO,
			machineIDs: []string{machine.ID},
			orderBy: &paginator.OrderBy{
				Field: "created",
				Order: paginator.OrderDescending,
			},
			firstEntry:    &mis[totalCount-1], // Last entry would have the highest created time
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {

			got, total, err := tc.dao.GetAll(
				ctx,
				nil,
				MachineInterfaceFilterInput{
					MachineIDs:             tc.machineIDs,
					ControllerInterfaceIDs: tc.controllerInterfaceIDs,
					ControllerSegmentIDs:   tc.controllerSegmentIDs,
					AttachedDpuMachineIDs:  tc.AttachedDPUMachineIDs,
					SubnetIDs:              tc.subnetIDs,
					Hostnames:              tc.hostnames,
				},
				paginator.PageInput{Offset: tc.offset, Limit: tc.limit, OrderBy: tc.orderBy},
				tc.paramRelations,
			)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				fmt.Println(err)
				assert.Equal(t, 0, len(got))
			} else {
				assert.Equal(t, tc.expectedCount, len(got))
				if len(tc.paramRelations) > 0 {
					assert.NotNil(t, got[0].Machine)
					assert.NotNil(t, got[0].Subnet)
				}
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.firstEntry != nil {
				assert.Equal(t, *tc.firstEntry.Hostname, *got[0].Hostname)
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestMachineInterfaceSQLDAO_UpdateFromParams(t *testing.T) {
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testMachineInterfaceSetupSchema(t, dbSession)

	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVPC")
	subnet := testInstanceBuildSubnet(t, dbSession, tenant, vpc, "testsubnet")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("mcTypeTest"))
	machine2 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("mcTypeTest2"))
	miDAO := NewMachineInterfaceDAO(dbSession)
	ctx := context.Background()
	controllerInterfaceID := uuid.New()
	controllerSegmentID := uuid.New()
	AttachedDPUMachineID := cutil.GetPtr(uuid.NewString())
	updatedIPAddresses := []string{"192.168.0.2, 172.168.0.2"}
	mi1, err := miDAO.Create(
		ctx, nil, MachineInterfaceCreateInput{MachineID: machine.ID, IsPrimary: true, IpAddresses: []string{}})
	assert.Nil(t, err)
	assert.NotNil(t, mi1)
	badSession, err := db.NewSession(context.Background(), "localhost", 1234, "postgres", "postgres", "postgres", "")
	assert.Nil(t, err)
	badDAO := NewMachineInterfaceDAO(badSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc string
		id   uuid.UUID
		dao  MachineInterfaceDAO

		paramMachineID             *string
		paramControllerInterfaceID *uuid.UUID
		paramControllerSegmentID   *uuid.UUID
		paramAttachedDPUMachineID  *string
		paramSubnetID              *uuid.UUID
		paramHostname              *string
		paramIsPrimary             *bool
		paramMacAddress            *string
		paramIPAddresses           []string

		expectedMachineID             *string
		expectedControllerInterfaceID *uuid.UUID
		expectedControllerSegmentID   *uuid.UUID
		expectedAttachedDPUMachineID  *string
		expectedSubnetID              *uuid.UUID
		expectedHostname              *string
		expectedIsPrimary             *bool
		expectedMacAddress            *string
		expectedIPAddresses           []string
		expectError                   bool
		verifyChildSpanner            bool
	}{
		{
			desc:               "error updating fields",
			id:                 mi1.ID,
			dao:                badDAO,
			expectError:        true,
			verifyChildSpanner: true,
		},
		{
			desc:        "can update all fields",
			id:          mi1.ID,
			dao:         miDAO,
			expectError: false,

			paramMachineID:             &machine2.ID,
			paramControllerInterfaceID: &controllerInterfaceID,
			paramControllerSegmentID:   &controllerSegmentID,
			paramAttachedDPUMachineID:  AttachedDPUMachineID,
			paramSubnetID:              &subnet.ID,
			paramHostname:              cutil.GetPtr("hostname"),
			paramIsPrimary:             cutil.GetPtr(false),
			paramMacAddress:            cutil.GetPtr("0:0:0:0:0:1"),
			paramIPAddresses:           updatedIPAddresses,

			expectedMachineID:             &machine2.ID,
			expectedControllerInterfaceID: &controllerInterfaceID,
			expectedControllerSegmentID:   &controllerSegmentID,
			expectedAttachedDPUMachineID:  AttachedDPUMachineID,
			expectedSubnetID:              &subnet.ID,
			expectedHostname:              cutil.GetPtr("hostname"),
			expectedIsPrimary:             cutil.GetPtr(false),
			expectedMacAddress:            cutil.GetPtr("0:0:0:0:0:1"),
			expectedIPAddresses:           updatedIPAddresses,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := tc.dao.Update(
				ctx,
				nil,
				MachineInterfaceUpdateInput{
					MachineInterfaceID:    tc.id,
					MachineID:             tc.paramMachineID,
					ControllerInterfaceID: tc.paramControllerInterfaceID,
					ControllerSegmentID:   tc.paramControllerSegmentID,
					AttachedDpuMachineID:  tc.paramAttachedDPUMachineID,
					SubnetID:              tc.paramSubnetID,
					Hostname:              tc.paramHostname,
					IsPrimary:             tc.paramIsPrimary,
					MacAddress:            tc.paramMacAddress,
					IpAddresses:           tc.paramIPAddresses,
				},
			)
			assert.Equal(t, tc.expectError, err != nil)

			if !tc.expectError {
				assert.Equal(t, *tc.expectedMachineID, got.MachineID)
				assert.Equal(t, *tc.expectedControllerInterfaceID, *got.ControllerInterfaceID)
				assert.Equal(t, *tc.expectedControllerSegmentID, *got.ControllerSegmentID)
				assert.Equal(t, *tc.expectedAttachedDPUMachineID, *got.AttachedDPUMachineID)
				assert.Equal(t, *tc.expectedSubnetID, *got.SubnetID)
				assert.Equal(t, *tc.expectedHostname, *got.Hostname)
				assert.Equal(t, *tc.expectedIsPrimary, got.IsPrimary)
				assert.Equal(t, *tc.expectedMacAddress, *got.MacAddress)
				assert.Equal(t, len(tc.expectedIPAddresses), len(got.IPAddresses))

				if got.Updated.String() == mi1.Updated.String() {
					t.Errorf("got.Updated = %v, want different value", got.Updated)
				}

			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestMachineInterfaceSQLDAO_Clear(t *testing.T) {
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testMachineInterfaceSetupSchema(t, dbSession)

	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVPC")
	subnet := testInstanceBuildSubnet(t, dbSession, tenant, vpc, "testsubnet")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("mcTypeTest"))
	miDAO := NewMachineInterfaceDAO(dbSession)
	ctx := context.Background()
	controllerInterfaceID := uuid.New()
	controllerSegmentID := uuid.New()
	AttachedDPUMachineID := cutil.GetPtr(uuid.NewString())
	updatedIPAddresses := []string{"192.168.0.2, 172.168.0.2"}
	mi1, err := miDAO.Create(
		ctx,
		nil,
		MachineInterfaceCreateInput{
			MachineID:             machine.ID,
			ControllerInterfaceID: &controllerInterfaceID,
			ControllerSegmentID:   &controllerSegmentID,
			AttachedDpuMachineID:  AttachedDPUMachineID,
			SubnetID:              &subnet.ID,
			Hostname:              cutil.GetPtr("hostname"),
			IsPrimary:             true,
			MacAddress:            cutil.GetPtr("0:0:0:0:0:1"),
			IpAddresses:           updatedIPAddresses,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, mi1)
	badSession, err := db.NewSession(context.Background(), "localhost", 1234, "postgres", "postgres", "postgres", "")
	assert.Nil(t, err)
	badDAO := NewMachineInterfaceDAO(badSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc string
		mi   *MachineInterface
		dao  MachineInterfaceDAO

		paramControllerInterfaceID bool
		paramControllerSegmentID   bool
		paramAttachedDPUMachineID  bool
		paramSubnetID              bool
		paramHostname              bool
		paramMacAddress            bool

		expectUpdate       bool
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:               "error clearing fields",
			mi:                 mi1,
			dao:                badDAO,
			expectUpdate:       false,
			expectError:        true,
			verifyChildSpanner: true,
		},
		{
			desc:         "can clear all fields",
			mi:           mi1,
			dao:          miDAO,
			expectUpdate: true,
			expectError:  false,

			paramControllerInterfaceID: true,
			paramControllerSegmentID:   true,
			paramAttachedDPUMachineID:  true,
			paramSubnetID:              true,
			paramHostname:              true,
			paramMacAddress:            true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := tc.dao.Clear(
				ctx,
				nil,
				MachineInterfaceClearInput{
					MachineInterfaceID:    tc.mi.ID,
					ControllerInterfaceID: tc.paramControllerInterfaceID,
					ControllerSegmentID:   tc.paramControllerSegmentID,
					AttachedDpuMachineID:  tc.paramAttachedDPUMachineID,
					SubnetID:              tc.paramSubnetID,
					Hostname:              tc.paramHostname,
					MacAddress:            tc.paramMacAddress,
				},
			)
			require.Equal(t, tc.expectError, err != nil, err)

			if !tc.expectError {
				assert.Nil(t, got.ControllerInterfaceID)
				assert.Nil(t, got.ControllerSegmentID)
				assert.Nil(t, got.AttachedDPUMachineID)
				assert.Nil(t, got.SubnetID)
				assert.Nil(t, got.Hostname)
				assert.Nil(t, got.MacAddress)
			}

			if tc.expectUpdate {
				assert.True(t, got.Updated.After(tc.mi.Updated))
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestMachineInterfaceSQLDAO_Delete(t *testing.T) {
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testMachineInterfaceSetupSchema(t, dbSession)

	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVPC")
	subnet := testInstanceBuildSubnet(t, dbSession, tenant, vpc, "testsubnet")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("mcTypeTest"))

	miDAO := NewMachineInterfaceDAO(dbSession)

	ctx := context.Background()
	mi, err := miDAO.Create(
		ctx,
		nil,
		MachineInterfaceCreateInput{
			MachineID:             machine.ID,
			ControllerInterfaceID: cutil.GetPtr(uuid.New()),
			ControllerSegmentID:   cutil.GetPtr(uuid.New()),
			AttachedDpuMachineID:  cutil.GetPtr(uuid.NewString()),
			SubnetID:              cutil.GetPtr(subnet.ID),
			Hostname:              cutil.GetPtr("hostname"),
			IsPrimary:             true,
			MacAddress:            cutil.GetPtr("0:0:0:0:0:0"),
			IpAddresses:           []string{"192.168.0.1, 172.168.0.1"},
		},
	)

	mi2, err := miDAO.Create(
		ctx,
		nil,
		MachineInterfaceCreateInput{
			MachineID:             machine.ID,
			ControllerInterfaceID: cutil.GetPtr(uuid.New()),
			ControllerSegmentID:   cutil.GetPtr(uuid.New()),
			AttachedDpuMachineID:  cutil.GetPtr(uuid.NewString()),
			SubnetID:              cutil.GetPtr(subnet.ID),
			Hostname:              cutil.GetPtr("hostname"),
			IsPrimary:             true,
			MacAddress:            cutil.GetPtr("0:0:0:0:0:0"),
			IpAddresses:           []string{"192.168.0.2, 172.168.0.2"},
		},
	)

	// Test with bad DB session
	badSession, err := db.NewSession(context.Background(), "localhost", 1234, "postgres", "postgres", "postgres", "")
	assert.Nil(t, err)
	badDAO := NewMachineInterfaceDAO(badSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		dao                MachineInterfaceDAO
		miID               uuid.UUID
		purge              bool
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "test deleting object failure, invalid session",
			dao:                badDAO,
			miID:               mi.ID,
			expectedError:      true,
			verifyChildSpanner: true,
		},
		{
			desc:          "test deleting existing object success",
			dao:           miDAO,
			miID:          mi.ID,
			expectedError: false,
		},
		{
			desc:          "test deleting non-existent object success",
			dao:           miDAO,
			miID:          uuid.New(),
			expectedError: false,
		},
		{
			desc:          "test deleting existing object success with purge",
			dao:           miDAO,
			miID:          mi2.ID,
			purge:         true,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.dao.Delete(ctx, nil, tc.miID, tc.purge)

			if tc.expectedError {
				assert.Error(t, err)

				// Check that object was not deleted
				tmp, serr := miDAO.GetByID(ctx, nil, tc.miID, nil)
				assert.NoError(t, serr)
				assert.NotNil(t, tmp)
				return
			}

			var res MachineInterface

			if tc.purge {
				err = dbSession.DB.NewSelect().Model(&res).Where("mi.id = ?", tc.miID).WhereAllWithDeleted().Scan(ctx)
			} else {
				err = dbSession.DB.NewSelect().Model(&res).Where("mi.id = ?", tc.miID).Scan(ctx)
			}
			assert.ErrorIs(t, err, sql.ErrNoRows)

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
