// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ipam

import (
	"context"
	"fmt"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbutil "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
)

// ~~~~~ For Testing IPAM ~~~~~ //

var (
	testIpamDB cipam.Storage
)

// getTestIpamer returns the test ipamer
func getTestIpamer(t *testing.T, ipamDB cipam.Storage) cipam.Ipamer {
	return cipam.NewWithStorage(ipamDB)
}

// getTestIpamDB returns the test ipam DB
func getTestIpamDB(t *testing.T, dbSession *db.Session, reset bool) cipam.Storage {
	if testIpamDB != nil {
		if reset {
			require.NoError(t, testIpamDB.DeleteAllPrefixes(context.Background(), ""))
		}
		return testIpamDB
	}

	storage := cipam.NewBunStorage(dbSession.DB, nil)

	// ensure the ipam schema is applied in test db
	require.NoError(t, storage.ApplyDbSchema())

	testIpamDB := NewIpamStorage(dbSession.DB, nil)
	if reset {
		require.NoError(t, testIpamDB.DeleteAllPrefixes(context.Background(), ""))
	}
	return testIpamDB
}

// reset the tables needed for IPBlock tests
func testIpamSetupSchema(t *testing.T, dbSession *db.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IPBlock)(nil))
	assert.Nil(t, err)
}

func testIpamBuildSite(t *testing.T, dbSession *db.Session, ip *cdbm.InfrastructureProvider, name string) *cdbm.Site {
	st := &cdbm.Site{
		ID:                          uuid.New(),
		Name:                        name,
		DisplayName:                 cutil.GetPtr("Test"),
		Org:                         "test",
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(db.GetCurTime()),
		Status:                      cdbm.SiteStatusPending,
		CreatedBy:                   uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	assert.Nil(t, err)
	return st
}

func testIpamBuildInfrastructureProvider(t *testing.T, dbSession *db.Session, name string) *cdbm.InfrastructureProvider {
	ip := &cdbm.InfrastructureProvider{
		ID:          uuid.New(),
		Name:        name,
		DisplayName: cutil.GetPtr("TestInfraProvider"),
		Org:         "test",
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	assert.Nil(t, err)
	return ip
}

func testIpamBuildTenant(t *testing.T, dbSession *db.Session, name string) *cdbm.Tenant {
	tenant := &cdbm.Tenant{
		ID:   uuid.New(),
		Name: name,
		Org:  "test",
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

func testIpamBuildIPBlock(t *testing.T, dbSession *db.Session, ipBlock *cdbm.IPBlock) *cdbm.IPBlock {
	_, err := dbSession.DB.NewInsert().Model(ipBlock).Exec(context.Background())
	assert.Nil(t, err)
	return ipBlock
}

func TestCreateIpamEntryForIPBlock(t *testing.T) {
	dbSession := cdbutil.GetTestDBSession(t, false)
	defer dbSession.Close()

	ipamDB := getTestIpamDB(t, dbSession, true)
	ctx := context.Background()
	tests := []struct {
		name                     string
		prefix                   string
		PrefixLength             int
		routingType              string
		infrastructureProviderID string
		siteID                   string
		expectedErr              bool
	}{
		{
			name:                     "success for one entry",
			prefix:                   "192.168.1.0",
			PrefixLength:             24,
			routingType:              "DatacenterOnly",
			infrastructureProviderID: "testIP",
			siteID:                   "testSite",
			expectedErr:              false,
		},
		{
			name:                     "error when cidr already exists",
			prefix:                   "192.168.1.0",
			PrefixLength:             24,
			routingType:              "DatacenterOnly",
			infrastructureProviderID: "testIP",
			siteID:                   "testSite",
			expectedErr:              true,
		},
		{
			name:                     "error when cidr clash",
			prefix:                   "192.168.1.2",
			PrefixLength:             24,
			routingType:              "DatacenterOnly",
			infrastructureProviderID: "testIP",
			siteID:                   "testSite",
			expectedErr:              true,
		},
		{
			name:                     "success when cidr clash but different namespace",
			prefix:                   "192.168.1.0",
			PrefixLength:             24,
			routingType:              "DatacenterOnly",
			infrastructureProviderID: "testIP2",
			siteID:                   "testSite",
			expectedErr:              false,
		},
		{
			name:                     "success when cidr dont clash",
			prefix:                   "192.168.2.0",
			PrefixLength:             24,
			routingType:              "DatacenterOnly",
			infrastructureProviderID: "testIP",
			siteID:                   "testSite",
			expectedErr:              false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			pref, err := CreateIpamEntryForIPBlock(ctx, ipamDB, tc.prefix, tc.PrefixLength, tc.routingType, tc.infrastructureProviderID, tc.siteID)
			assert.Equal(t, tc.expectedErr, err != nil)
			if err == nil {
				assert.NotNil(t, pref)
				fmt.Println(pref.Namespace, pref.String())
			}
		})
	}
}

func TestDeleteIpamEntryForIPBlock(t *testing.T) {
	dbSession := cdbutil.GetTestDBSession(t, false)
	defer dbSession.Close()
	ipamDB := getTestIpamDB(t, dbSession, true)
	ctx := context.Background()
	_, err := CreateIpamEntryForIPBlock(ctx, ipamDB, "192.168.0.0", 24, "DatacenterOnly", "testIPDel", "testSite")
	assert.Nil(t, err)
	_, err = CreateIpamEntryForIPBlock(ctx, ipamDB, "192.168.1.0", 24, "DatacenterOnly", "testIP2Del", "testSite")
	assert.Nil(t, err)
	tests := []struct {
		name                     string
		prefix                   string
		PrefixLength             int
		routingType              string
		infrastructureProviderID string
		siteID                   string
		expectedErr              bool
	}{
		{
			name:                     "success deleting one entry",
			prefix:                   "192.168.0.0",
			PrefixLength:             24,
			routingType:              "DatacenterOnly",
			infrastructureProviderID: "testIPDel",
			siteID:                   "testSite",
			expectedErr:              false,
		},
		{
			name:                     "success when deleting entry doesnt exist",
			prefix:                   "192.168.0.0",
			PrefixLength:             24,
			routingType:              "DatacenterOnly",
			infrastructureProviderID: "testIPDel",
			siteID:                   "testSite",
			expectedErr:              false,
		},
		{
			name:                     "success deleting another entry",
			prefix:                   "192.168.1.0",
			PrefixLength:             24,
			routingType:              "DatacenterOnly",
			infrastructureProviderID: "testIP2Del",
			siteID:                   "testSite",
			expectedErr:              false,
		},
		{
			name:                     "success when deleting entry namespace doesnt exist",
			prefix:                   "192.168.0.0",
			PrefixLength:             24,
			routingType:              "DatacenterOnly",
			infrastructureProviderID: "testIPNone",
			siteID:                   "testSite",
			expectedErr:              false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			err := DeleteIpamEntryForIPBlock(ctx, ipamDB, tc.prefix, tc.PrefixLength, tc.routingType, tc.infrastructureProviderID, tc.siteID)
			assert.Equal(t, tc.expectedErr, err != nil)
		})
	}
}

func TestGetIpamUsageForIPBlock(t *testing.T) {
	dbSession := cdbutil.GetTestDBSession(t, false)
	defer dbSession.Close()

	ipamDB := getTestIpamDB(t, dbSession, true)
	ctx := context.Background()

	testIpamSetupSchema(t, dbSession)

	ip := testIpamBuildInfrastructureProvider(t, dbSession, "testip")
	site := testIpamBuildSite(t, dbSession, ip, "testsite")

	ipID := ip.ID
	siteID := site.ID

	ipBlock1 := &cdbm.IPBlock{
		ID:                       uuid.New(),
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: ipID,
		SiteID:                   siteID,
		Prefix:                   "192.168.0.0",
		PrefixLength:             16,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}

	ipBlock2 := &cdbm.IPBlock{
		ID:                       uuid.New(),
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: ipID,
		SiteID:                   siteID,
		Prefix:                   "192.169.1.0",
		PrefixLength:             28,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
		FullGrant:                true,
	}

	ipBlock3 := &cdbm.IPBlock{
		ID:                       uuid.New(),
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: ipID,
		SiteID:                   siteID,
		Prefix:                   "192.162.1.0",
		PrefixLength:             16,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}

	ipbPrefix1, err := CreateIpamEntryForIPBlock(ctx, ipamDB, "192.168.0.0", 16, "DatacenterOnly", ipID.String(), siteID.String())
	assert.Nil(t, err)

	ipbPrefix2, err := CreateIpamEntryForIPBlock(ctx, ipamDB, "192.169.1.0", 28, "DatacenterOnly", ipID.String(), siteID.String())
	assert.Nil(t, err)

	tests := []struct {
		name             string
		inputIPB         *cdbm.IPBlock
		inputPrefix      *cipam.Prefix
		expectedErr      bool
		exptectFullGrant bool
	}{
		{
			name:        "success nil IPBlock",
			inputIPB:    nil,
			inputPrefix: nil,
			expectedErr: true,
		},
		{
			name:        "success no prefix found for IPBlock",
			inputIPB:    ipBlock3,
			inputPrefix: nil,
			expectedErr: true,
		},
		{
			name:        "success get usage for IPBlock 1",
			inputIPB:    ipBlock1,
			inputPrefix: ipbPrefix1,
			expectedErr: false,
		},
		{
			name:             "success get usage for full grant IPBlock 2",
			inputIPB:         ipBlock2,
			inputPrefix:      ipbPrefix2,
			expectedErr:      false,
			exptectFullGrant: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			resUsage, err := GetIpamUsageForIPBlock(ctx, ipamDB, tc.inputIPB)
			assert.Equal(t, tc.expectedErr, err != nil)
			if !tc.expectedErr {
				assert.NotNil(t, resUsage)
				if tc.exptectFullGrant {
					assert.Equal(t, int(resUsage.AcquiredIPs), 0)
					assert.Equal(t, int(resUsage.AcquiredPrefixes), 1)
					assert.Equal(t, len(resUsage.AvailablePrefixes), 0)
					assert.Equal(t, int(resUsage.AvailableSmallestPrefixes), 0)

				} else {
					assert.Equal(t, int(resUsage.AcquiredIPs), int(tc.inputPrefix.Usage().AcquiredIPs))
					assert.Equal(t, int(resUsage.AcquiredPrefixes), int(tc.inputPrefix.Usage().AcquiredPrefixes))
					assert.Equal(t, len(resUsage.AvailablePrefixes), len(tc.inputPrefix.Usage().AvailablePrefixes))
					assert.Equal(t, int(resUsage.AvailableSmallestPrefixes), int(tc.inputPrefix.Usage().AvailableSmallestPrefixes))
				}
			}
		})
	}
}

func TestCreateChildIpamEntryForIPBlock(t *testing.T) {
	dbSession := cdbutil.GetTestDBSession(t, false)
	defer dbSession.Close()
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	ipamDB := getTestIpamDB(t, dbSession, true)
	ctx := context.Background()
	testIpamSetupSchema(t, dbSession)

	ip := testIpamBuildInfrastructureProvider(t, dbSession, "testip")
	site := testIpamBuildSite(t, dbSession, ip, "testsite")
	tenant := testIpamBuildTenant(t, dbSession, "testtenant")

	ipID := ip.ID
	siteID := site.ID
	tenantID := tenant.ID

	ipBlock1 := &cdbm.IPBlock{
		ID:                       uuid.New(),
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: ipID,
		SiteID:                   siteID,
		TenantID:                 &tenantID,
		Prefix:                   "192.168.0.0",
		PrefixLength:             16,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}
	ipBlock2 := &cdbm.IPBlock{
		ID:                       uuid.New(),
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: ipID,
		SiteID:                   siteID,
		TenantID:                 &tenantID,
		Prefix:                   "192.169.1.0",
		PrefixLength:             28,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}
	ipBlock3 := &cdbm.IPBlock{
		ID:                       uuid.New(),
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: uuid.New(),
		SiteID:                   uuid.New(),
		TenantID:                 &tenantID,
		Prefix:                   "192.169.0.0",
		PrefixLength:             16,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}
	ipBlock4 := &cdbm.IPBlock{
		ID:                       uuid.New(),
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: uuid.New(),
		SiteID:                   uuid.New(),
		TenantID:                 &tenantID,
		Prefix:                   "192.169.0.0",
		PrefixLength:             16,
		FullGrant:                true,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}
	ipBlock5 := &cdbm.IPBlock{
		ID:                       uuid.New(),
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: ipID,
		SiteID:                   siteID,
		TenantID:                 &tenantID,
		Prefix:                   "192.170.0.0",
		PrefixLength:             16,
		FullGrant:                false,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}
	ipBlock6 := &cdbm.IPBlock{
		ID:                       uuid.New(),
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: ipID,
		SiteID:                   siteID,
		TenantID:                 &tenantID,
		Prefix:                   "192.171.0.0",
		PrefixLength:             16,
		FullGrant:                false,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}
	siteFabricIPBlock := *ipBlock1
	siteFabricIPBlock.TenantID = nil

	ipamer := cipam.NewWithStorage(ipamDB)
	ipamer.SetNamespace(GetIpamNamespaceForIPBlock(ctx, cdbm.IPBlockRoutingTypeDatacenterOnly, ipID.String(), siteID.String()))
	prefix, err := ipamer.NewPrefix(ctx, "192.168.0.0/16")
	assert.Nil(t, err)
	assert.Equal(t, "192.168.0.0/16", prefix.Cidr)
	prefix, err = ipamer.NewPrefix(ctx, "192.169.1.0/28")
	assert.Nil(t, err)
	assert.Equal(t, "192.169.1.0/28", prefix.Cidr)
	prefix, err = ipamer.NewPrefix(ctx, "192.170.0.0/16")
	assert.Nil(t, err)
	assert.Equal(t, "192.170.0.0/16", prefix.Cidr)
	childPrefix, err := ipamer.AcquireChildPrefix(ctx, "192.170.0.0/16", uint8(24))
	assert.Nil(t, err)
	assert.NotNil(t, childPrefix)

	ipb6 := testIpamBuildIPBlock(t, dbSession, ipBlock6)
	assert.NotNil(t, ipb6)
	ipbDAO := cdbm.NewIPBlockDAO(dbSession)
	prefix, err = ipamer.NewPrefix(ctx, "192.171.0.0/16")
	assert.Nil(t, err)
	assert.Equal(t, "192.171.0.0/16", prefix.Cidr)

	tests := []struct {
		name              string
		parentIPBlock     *cdbm.IPBlock
		tx                *db.Tx
		childCount        int
		childPrefixLength int
		expectedErr       bool
		expectedError     string
		checkFullGrant    bool
	}{

		{
			name:              "success creating one child entry",
			parentIPBlock:     ipBlock1,
			childCount:        1,
			childPrefixLength: 24,
			expectedErr:       false,
		},
		{
			name:              "success creating multiple child entries",
			parentIPBlock:     ipBlock1,
			childCount:        5,
			childPrefixLength: 24,
			expectedErr:       false,
		},
		{
			name:              "failure when there is no space for child block",
			parentIPBlock:     ipBlock2,
			childCount:        1,
			childPrefixLength: 24,
			expectedErr:       true,
		},
		{
			name:              "failure when parent doesnt exist",
			parentIPBlock:     ipBlock3,
			childCount:        1,
			childPrefixLength: 24,
			expectedErr:       true,
		},
		{
			name:          "failure when parent is nil",
			parentIPBlock: nil,
			expectedErr:   true,
			childCount:    1,
		},
		{
			name:              "error in full grant when parent prefix does not exist",
			parentIPBlock:     ipBlock3,
			childCount:        1,
			childPrefixLength: 16,
			expectedErr:       true,
		},
		{
			name:          "failure when parent is fully granted already",
			parentIPBlock: ipBlock4,
			expectedErr:   true,
			childCount:    1,
		},
		{
			name:              "failure when full-grant is required but parent IPBlock has acquired prefix already",
			parentIPBlock:     ipBlock5,
			expectedErr:       true,
			childPrefixLength: 16,
			childCount:        1,
		},
		{
			name:              "success when full-grant is required",
			parentIPBlock:     ipb6,
			expectedErr:       false,
			childPrefixLength: 16,
			childCount:        1,
			checkFullGrant:    true,
		},
		{
			name:              "failure when a Site fabric parent has no transaction",
			parentIPBlock:     &siteFabricIPBlock,
			expectedErr:       true,
			expectedError:     fmt.Sprintf("parent IP Block %s requires a transaction for allocation", siteFabricIPBlock.ID),
			childPrefixLength: 24,
			childCount:        1,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			for i := 0; i < tc.childCount; i++ {
				pref, err := CreateChildIpamEntryForIPBlock(ctx, nil, dbSession, ipamDB, tc.parentIPBlock, tc.childPrefixLength)
				assert.Equal(t, tc.expectedErr, err != nil)
				if tc.expectedError != "" {
					assert.EqualError(t, err, tc.expectedError)
				}
				if !tc.expectedErr {
					assert.NotNil(t, pref)
					fmt.Println(pref.Cidr)
				} else {
					fmt.Println(err)
				}
				if tc.checkFullGrant {
					assert.Equal(t, true, tc.parentIPBlock.FullGrant)
					ipb, err := ipbDAO.GetByID(ctx, nil, tc.parentIPBlock.ID, nil)
					assert.Nil(t, err)
					assert.Equal(t, true, ipb.FullGrant)
					assert.Equal(t, pref.Cidr, GetCidrForIPBlock(ctx, tc.parentIPBlock.Prefix, tc.parentIPBlock.PrefixLength))
				}
			}
		})
	}

	t.Run("database reload error has a stable classification", func(t *testing.T) {
		tx, err := db.BeginTx(ctx, dbSession, nil)
		require.NoError(t, err)
		defer func() {
			assert.NoError(t, tx.Rollback())
		}()

		missing := siteFabricIPBlock
		missing.ID = uuid.New()
		_, err = CreateChildIpamEntryForIPBlock(ctx, tx, dbSession, ipamDB, &missing, 24)
		require.ErrorIs(t, err, ErrParentIPBlockReload)
		require.ErrorIs(t, err, db.ErrDoesNotExist)
	})
}

func TestLockAndValidateParentIPBlockForAllocation(t *testing.T) {
	dbSession := cdbutil.GetTestDBSession(t, false)
	defer dbSession.Close()
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	ctx := context.Background()
	testIpamSetupSchema(t, dbSession)

	ip := testIpamBuildInfrastructureProvider(t, dbSession, "lock-parent-ip-block")
	site := testIpamBuildSite(t, dbSession, ip, "lock-parent-ip-block")
	tests := []struct {
		name          string
		storedStatus  string
		callerStatus  string
		expectedError bool
	}{
		{
			name:         "refreshes a stale caller from the locked Ready row",
			storedStatus: cdbm.IPBlockStatusReady,
			callerStatus: cdbm.IPBlockStatusDeleting,
		},
		{
			name:          "rejects the current locked Deleting row",
			storedStatus:  cdbm.IPBlockStatusDeleting,
			callerStatus:  cdbm.IPBlockStatusReady,
			expectedError: true,
		},
	}

	for index, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			linkedID := uuid.New()
			linked := testIpamBuildIPBlock(t, dbSession, &cdbm.IPBlock{
				ID:                       uuid.New(),
				Name:                     fmt.Sprintf("linked-site-fabric-parent-%d", index),
				InfrastructureProviderID: ip.ID,
				SiteID:                   site.ID,
				SitePrefixID:             &linkedID,
				RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
				Prefix:                   fmt.Sprintf("192.172.%d.0", index),
				PrefixLength:             24,
				ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
				Status:                   tc.storedStatus,
			})
			stale := *linked
			stale.Status = tc.callerStatus

			tx, err := db.BeginTx(ctx, dbSession, nil)
			require.NoError(t, err)
			defer func() {
				assert.NoError(t, tx.Rollback())
			}()
			err = LockAndValidateParentIPBlockForAllocation(ctx, tx, dbSession, &stale)
			if tc.expectedError {
				assert.EqualError(t, err, fmt.Sprintf("parent IP Block %s linked to an OperatorManaged SitePrefix is not Ready", linked.ID))
			} else {
				require.NoError(t, err)
			}
			assert.Equal(t, tc.storedStatus, stale.Status)
		})
	}
}

func TestParseCidrIntoPrefixAndPrefixLength(t *testing.T) {
	tests := []struct {
		name           string
		cidr           string
		expectedErr    bool
		expectedPrefix string
		expectedBits   int
	}{
		{
			name:           "success ipv4",
			cidr:           "192.168.1.1/24",
			expectedErr:    false,
			expectedPrefix: "192.168.1.0",
			expectedBits:   24,
		},
		{
			name:           "success ipv6",
			cidr:           "2001:aabb::/48",
			expectedErr:    false,
			expectedPrefix: "2001:aabb::",
			expectedBits:   48,
		},
		{
			name:           "error ipv4",
			cidr:           "192.168.0.330/24",
			expectedErr:    true,
			expectedPrefix: "",
			expectedBits:   0,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			p, bits, err := ParseCidrIntoPrefixAndBlockSize(tc.cidr)
			assert.Equal(t, tc.expectedErr, err != nil)
			assert.Equal(t, tc.expectedPrefix, p)
			assert.Equal(t, tc.expectedBits, bits)
		})
	}
}

func TestDeleteChildIpamEntryFromCidr(t *testing.T) {
	dbSession := cdbutil.GetTestDBSession(t, false)
	defer dbSession.Close()
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	ipamDB := getTestIpamDB(t, dbSession, true)
	ctx := context.Background()
	testIpamSetupSchema(t, dbSession)

	ip := testIpamBuildInfrastructureProvider(t, dbSession, "testip")
	site := testIpamBuildSite(t, dbSession, ip, "testsite")
	tenant := testIpamBuildTenant(t, dbSession, "testtenant")

	ipID := ip.ID
	siteID := site.ID
	tenantID := tenant.ID

	ipBlock1 := &cdbm.IPBlock{
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: ipID,
		SiteID:                   siteID,
		TenantID:                 &tenantID,
		Prefix:                   "192.168.0.0",
		PrefixLength:             16,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}
	ipamer := cipam.NewWithStorage(ipamDB)
	ipamer.SetNamespace(GetIpamNamespaceForIPBlock(ctx, cdbm.IPBlockRoutingTypeDatacenterOnly, ipID.String(), siteID.String()))
	prefix, err := ipamer.NewPrefix(ctx, "192.168.0.0/16")
	assert.Nil(t, err)
	assert.Equal(t, "192.168.0.0/16", prefix.Cidr)
	childPrefix, err := CreateChildIpamEntryForIPBlock(ctx, nil, dbSession, ipamDB, ipBlock1, 24)
	assert.Nil(t, err)

	ipBlock6 := &cdbm.IPBlock{
		ID:                       uuid.New(),
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: ipID,
		SiteID:                   siteID,
		TenantID:                 &tenantID,
		Prefix:                   "192.170.0.0",
		PrefixLength:             16,
		FullGrant:                true,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}
	ipb6 := testIpamBuildIPBlock(t, dbSession, ipBlock6)
	assert.NotNil(t, ipb6)
	ipbDAO := cdbm.NewIPBlockDAO(dbSession)

	tests := []struct {
		name           string
		parentIPBlock  *cdbm.IPBlock
		tx             *db.Tx
		childCidr      string
		expectedErr    bool
		checkFullGrant bool
	}{

		{
			name:          "success when cidr exists",
			parentIPBlock: ipBlock1,
			childCidr:     childPrefix.Cidr,
			expectedErr:   false,
		},
		{
			name:          "failure when cidr doesnt exist",
			parentIPBlock: ipBlock1,
			childCidr:     "172.10.10.10/28",
			expectedErr:   true,
		},
		{
			name:          "failure when parent is nil",
			parentIPBlock: nil,
			expectedErr:   true,
		},
		{
			name:           "failure when full-grant but child cidr doesnt match",
			parentIPBlock:  ipb6,
			expectedErr:    true,
			childCidr:      "192.170.0.0/24",
			checkFullGrant: false,
		},
		{
			name:           "success when full-grant",
			parentIPBlock:  ipb6,
			expectedErr:    false,
			childCidr:      "192.170.0.0/16",
			checkFullGrant: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			err := DeleteChildIpamEntryFromCidr(ctx, nil, dbSession, ipamDB, tc.parentIPBlock, tc.childCidr)
			assert.Equal(t, tc.expectedErr, err != nil)
			if !tc.expectedErr {
				// verify that prefix was indeed deleted
				p := ipamer.PrefixFrom(ctx, tc.childCidr)
				assert.Nil(t, p)
			} else {
				fmt.Println(err)
			}
			if tc.checkFullGrant {
				assert.Equal(t, false, tc.parentIPBlock.FullGrant)
				ipb, err := ipbDAO.GetByID(ctx, nil, tc.parentIPBlock.ID, nil)
				assert.Nil(t, err)
				assert.Equal(t, false, ipb.FullGrant)
			}
		})
	}
}

func TestAcquireSpecificChildIpamEntryForIPBlock(t *testing.T) {
	dbSession := cdbutil.GetTestDBSession(t, false)
	defer dbSession.Close()
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	ipamDB := getTestIpamDB(t, dbSession, true)
	ctx := context.Background()
	testIpamSetupSchema(t, dbSession)

	ip := testIpamBuildInfrastructureProvider(t, dbSession, "testip-specific")
	site := testIpamBuildSite(t, dbSession, ip, "testsite-specific")

	parent := &cdbm.IPBlock{
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		TenantID:                 cutil.GetPtr(uuid.New()),
		Prefix:                   "10.20.0.0",
		PrefixLength:             16,
		FullGrant:                false,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}
	ipamer := cipam.NewWithStorage(ipamDB)
	ipamer.SetNamespace(GetIpamNamespaceForIPBlock(ctx, parent.RoutingType, parent.InfrastructureProviderID.String(), parent.SiteID.String()))
	prefix, err := ipamer.NewPrefix(ctx, "10.20.0.0/16")
	assert.Nil(t, err)
	assert.Equal(t, "10.20.0.0/16", prefix.Cidr)

	fullGrantParent := &cdbm.IPBlock{
		ID:                       uuid.New(),
		RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		TenantID:                 cutil.GetPtr(uuid.New()),
		Prefix:                   "10.21.0.0",
		PrefixLength:             16,
		FullGrant:                true,
		ProtocolVersion:          cdbm.IPBlockProtocolVersionV4,
	}
	siteFabricParent := *parent
	siteFabricParent.ID = uuid.New()
	siteFabricParent.TenantID = nil

	tests := []struct {
		name          string
		parentIPBlock *cdbm.IPBlock
		childCidr     string
		expectedErr   bool
		expectedError string
	}{
		{
			name:          "success acquiring specific child",
			parentIPBlock: parent,
			childCidr:     "10.20.1.0/24",
			expectedErr:   false,
		},
		{
			name:          "failure when child equals parent",
			parentIPBlock: parent,
			childCidr:     "10.20.0.0/16",
			expectedErr:   true,
			expectedError: fmt.Sprintf(
				"child CIDR 10.20.0.0/16 equals parent CIDR 10.20.0.0/16 for IPBlock %s; use CreateChildIpamEntryForIPBlock",
				parent.ID,
			),
		},
		{
			name:          "failure when parent is fully granted",
			parentIPBlock: fullGrantParent,
			childCidr:     "10.21.1.0/24",
			expectedErr:   true,
			expectedError: fmt.Sprintf("parent IPBlock %s already has a full grant", fullGrantParent.ID),
		},
		{
			name:          "failure when parent is nil",
			parentIPBlock: nil,
			childCidr:     "10.20.2.0/24",
			expectedErr:   true,
		},
		{
			name:          "failure when a Site fabric parent has no transaction",
			parentIPBlock: &siteFabricParent,
			childCidr:     "10.20.2.0/24",
			expectedErr:   true,
			expectedError: fmt.Sprintf("parent IP Block %s requires a transaction for allocation", siteFabricParent.ID),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			child, err := AcquireSpecificChildIpamEntryForIPBlock(ctx, nil, dbSession, ipamDB, tc.parentIPBlock, tc.childCidr)
			assert.Equal(t, tc.expectedErr, err != nil)
			if tc.expectedError != "" {
				assert.EqualError(t, err, tc.expectedError)
			}
			if tc.expectedErr {
				assert.Nil(t, child)
				return
			}
			assert.NotNil(t, child)
			assert.Equal(t, tc.childCidr, child.Cidr)
		})
	}
}

// Generic IPAM library tests from the api
func TestIpamer_NewPrefix(t *testing.T) {
	// test ipam operations from api
	ctx := context.Background()
	dbSession := cdbutil.GetTestDBSession(t, false)
	defer dbSession.Close()
	ipamDB := getTestIpamDB(t, dbSession, true)
	ipamer := cipam.NewWithStorage(ipamDB)
	ipamer.SetNamespace("TestNewPrefix")

	prefix, err := ipamer.NewPrefix(ctx, "192.168.0.0/24")
	assert.Nil(t, err)
	ip1, err := ipamer.AcquireIP(ctx, prefix.Cidr)
	assert.Nil(t, err)
	ip2, err := ipamer.AcquireIP(ctx, prefix.Cidr)
	assert.Nil(t, err)

	assert.Equal(t, "192.168.0.0/24", prefix.String())
	assert.Equal(t, "192.168.0.1", ip1.IP.String())

	assert.Equal(t, "192.168.0.0/24", ip1.ParentPrefix)
	assert.Equal(t, "192.168.0.2", ip2.IP.String())
	assert.Equal(t, "192.168.0.0/24", ip2.ParentPrefix)

	_, err = ipamer.ReleaseIP(ctx, ip2)
	assert.Nil(t, err)

	_, err = ipamer.ReleaseIP(ctx, ip1)
	assert.Nil(t, err)

	_, err = ipamer.DeletePrefix(ctx, prefix.Cidr)
	assert.Nil(t, err)
}

func TestIpamer_AcquireChildPrefixV6(t *testing.T) {
	// test ipam operations from api
	ctx := context.Background()
	dbSession := cdbutil.GetTestDBSession(t, false)
	defer dbSession.Close()
	ipamDB := getTestIpamDB(t, dbSession, true)
	ipamer := cipam.NewWithStorage(ipamDB)
	ipamer.SetNamespace("TestAcquireChildPrefix")

	prefix, err := ipamer.NewPrefix(ctx, "2001:aabb::/48")
	assert.Nil(t, err)
	cp1, err := ipamer.AcquireChildPrefix(ctx, prefix.Cidr, 64)
	assert.Nil(t, err)
	cp2, err := ipamer.AcquireChildPrefix(ctx, prefix.Cidr, 72)
	assert.Nil(t, err)
	ip21, err := ipamer.AcquireIP(ctx, cp2.Cidr)
	assert.Nil(t, err)
	prefix = ipamer.PrefixFrom(ctx, prefix.Cidr)

	assert.Equal(t, "2001:aabb::/48", prefix.String())
	assert.Equal(t, "2001:aabb::/64", cp1.String())
	assert.Equal(t, "2001:aabb:0:1::/72", cp2.String())
	assert.Equal(t, "2001:aabb:0:1::1", ip21.IP.String())

	assert.Nil(t, ipamer.ReleaseChildPrefix(ctx, cp1))
	_, err = ipamer.ReleaseIP(ctx, ip21)
	assert.Nil(t, err)
	err = ipamer.ReleaseChildPrefix(ctx, cp2)
	assert.Nil(t, err)
	_, err = ipamer.DeletePrefix(ctx, prefix.Cidr)
	assert.Nil(t, err)
}

func TestIpamer_AcquireChildPrefixV4(t *testing.T) {
	// test ipam operations from api
	ctx := context.Background()
	dbSession := cdbutil.GetTestDBSession(t, false)
	defer dbSession.Close()
	ipamDB := getTestIpamDB(t, dbSession, true)
	ipamer := cipam.NewWithStorage(ipamDB)
	ipamer.SetNamespace("TestAcquireChildPrefixV4")

	prefix, err := ipamer.NewPrefix(ctx, "192.168.0.0/16")
	assert.Nil(t, err)
	cp1, err := ipamer.AcquireChildPrefix(ctx, prefix.Cidr, 24)
	assert.Nil(t, err)
	fmt.Println(cp1.Cidr)
	cp2, err := ipamer.AcquireChildPrefix(ctx, prefix.Cidr, 24)
	assert.Nil(t, err)
	ip21, err := ipamer.AcquireIP(ctx, cp2.Cidr)
	assert.Nil(t, err)
	fmt.Println(cp2.Cidr)
	prefix = ipamer.PrefixFrom(ctx, prefix.Cidr)
	assert.Nil(t, ipamer.ReleaseChildPrefix(ctx, cp1))
	_, err = ipamer.ReleaseIP(ctx, ip21)
	assert.Nil(t, err)
	err = ipamer.ReleaseChildPrefix(ctx, cp2)
	assert.Nil(t, err)
	_, err = ipamer.DeletePrefix(ctx, prefix.Cidr)
	assert.Nil(t, err)
}

func TestGetFirstIPFromCidr(t *testing.T) {
	tests := []struct {
		name        string
		cidr        string
		expectedErr bool
		expectedIP  string
	}{
		{
			name:        "success",
			cidr:        "192.168.1.0/24",
			expectedErr: false,
			expectedIP:  "192.168.1.1",
		},
		{
			name:        "fail",
			cidr:        "badcidr",
			expectedErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			ip, err := GetFirstIPFromCidr(tc.cidr)
			assert.Equal(t, tc.expectedErr, err != nil)
			if err == nil {
				assert.Equal(t, tc.expectedIP, ip)
			}
		})
	}
}
