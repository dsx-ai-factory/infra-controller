// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"net/netip"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/extra/bundebug"
)

func testIPBlockInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// reset the tables needed for IPBlock tests
func testIPBlockSetupSchema(t *testing.T, dbSession *db.Session) {
	// create Site table
	err := dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	assert.Nil(t, err)
	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*IPBlock)(nil))
	assert.Nil(t, err)
}

func testIPBlockBuildSite(t *testing.T, dbSession *db.Session, ip *InfrastructureProvider, name string) *Site {
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

func testIPBlockBuildInfrastructureProvider(t *testing.T, dbSession *db.Session, name string) *InfrastructureProvider {
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

func testIPBlockBuildTenant(t *testing.T, dbSession *db.Session, name string) *Tenant {
	tenant := &Tenant{
		ID:   uuid.New(),
		Name: name,
		Org:  "test",
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

func TestIPBlock_ContainsPrefix(t *testing.T) {
	tests := []struct {
		name             string
		ipBlock          *IPBlock
		reportedPrefix   string
		expectedContains bool
	}{
		{
			name:             "contains a narrower IPv4 prefix",
			ipBlock:          &IPBlock{Prefix: "10.20.0.0", PrefixLength: 16},
			reportedPrefix:   "10.20.30.0/24",
			expectedContains: true,
		},
		{
			name:             "rejects an IPv4 prefix outside the block",
			ipBlock:          &IPBlock{Prefix: "10.20.0.0", PrefixLength: 16},
			reportedPrefix:   "10.21.30.0/24",
			expectedContains: false,
		},
		{
			name:             "rejects a reported prefix wider than the block",
			ipBlock:          &IPBlock{Prefix: "10.20.16.0", PrefixLength: 20},
			reportedPrefix:   "10.20.0.0/16",
			expectedContains: false,
		},
		{
			name:             "rejects a different address family",
			ipBlock:          &IPBlock{Prefix: "10.20.0.0", PrefixLength: 16},
			reportedPrefix:   "2001:db8::/64",
			expectedContains: false,
		},
		{
			name:             "rejects an invalid IPBlock prefix",
			ipBlock:          &IPBlock{Prefix: "not-an-address", PrefixLength: 16},
			reportedPrefix:   "10.20.30.0/24",
			expectedContains: false,
		},
		{
			name:             "rejects a nil IPBlock",
			ipBlock:          nil,
			reportedPrefix:   "10.20.30.0/24",
			expectedContains: false,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			reportedPrefix, err := netip.ParsePrefix(test.reportedPrefix)
			require.NoError(t, err)
			assert.Equal(t, test.expectedContains, test.ipBlock.ContainsPrefix(reportedPrefix))
		})
	}
}

func TestIPBlockSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()
	testIPBlockSetupSchema(t, dbSession)
	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testIPBlockBuildSite(t, dbSession, ip, "testSite")
	tenant := testIPBlockBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	ipsd := NewIPBlockDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)
	sitePrefixID := uuid.New()

	tests := []struct {
		desc               string
		its                []IPBlock
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one with SitePrefix ID",
			its: []IPBlock{
				{
					Name:                     "test",
					SiteID:                   site.ID,
					InfrastructureProviderID: ip.ID,
					TenantID:                 &tenant.ID,
					SitePrefixID:             &sitePrefixID,
					PrefixLength:             32,
					Prefix:                   "10.0.1.0",
					Status:                   IPBlockStatusPending,
					CreatedBy:                &user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple, some with nullable site field",
			its: []IPBlock{
				{
					Name: "test1", SiteID: site.ID, InfrastructureProviderID: ip.ID, TenantID: &tenant.ID, PrefixLength: 32, Prefix: "10.0.1.0", FullGrant: false, Status: IPBlockStatusPending, CreatedBy: &user.ID,
				},
				{
					Name: "test2", SiteID: site.ID, InfrastructureProviderID: ip.ID, TenantID: &tenant.ID, PrefixLength: 32, Prefix: "10.0.2.0", FullGrant: false, Status: IPBlockStatusPending, CreatedBy: &user.ID,
				},
				{
					Name: "test3", SiteID: site.ID, InfrastructureProviderID: ip.ID, TenantID: &tenant.ID, PrefixLength: 32, Prefix: "10.0.3.0", FullGrant: true, Status: IPBlockStatusPending, CreatedBy: &user.ID,
				},
			},
			expectError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.its {
				it, err := ipsd.Create(
					ctx,
					nil,
					IPBlockCreateInput{
						Name:                     i.Name,
						Description:              cutil.GetPtr("description"),
						SiteID:                   site.ID,
						InfrastructureProviderID: ip.ID,
						TenantID:                 &tenant.ID,
						SitePrefixID:             i.SitePrefixID,
						RoutingType:              IPBlockRoutingTypePublic,
						Prefix:                   i.Prefix,
						PrefixLength:             i.PrefixLength,
						ProtocolVersion:          "v4",
						FullGrant:                i.FullGrant,
						Status:                   i.Status,
						CreatedBy:                i.CreatedBy,
					},
				)
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					require.NotNil(t, it)
					assert.Equal(t, i.SitePrefixID, it.SitePrefixID)
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

func TestIPBlockSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()
	testIPBlockSetupSchema(t, dbSession)
	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "testIP")
	site1 := testIPBlockBuildSite(t, dbSession, ip, "testSite1")
	tenant := testIPBlockBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	ipsd := NewIPBlockDAO(dbSession)

	ipb, err := ipsd.Create(
		ctx,
		nil,
		IPBlockCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("description"),
			SiteID:                   site1.ID,
			InfrastructureProviderID: ip.ID,
			TenantID:                 &tenant.ID,
			RoutingType:              IPBlockRoutingTypePublic,
			Prefix:                   "10.0.1.0",
			PrefixLength:             32,
			ProtocolVersion:          "v4",
			FullGrant:                false,
			Status:                   IPBlockStatusProvisioning,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ipb)

	ipb2, err := ipsd.Create(
		ctx, nil, IPBlockCreateInput{
			Name:                     "test2",
			Description:              cutil.GetPtr("description"),
			SiteID:                   site1.ID,
			InfrastructureProviderID: ip.ID,
			RoutingType:              IPBlockRoutingTypePublic,
			Prefix:                   "10.0.2.0",
			PrefixLength:             32,
			ProtocolVersion:          "v4",
			FullGrant:                false,
			Status:                   IPBlockStatusProvisioning,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ipb2)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                           string
		id                             uuid.UUID
		ipb                            *IPBlock
		paramRelations                 []string
		expectedError                  bool
		expectedErrVal                 error
		expectedSite                   bool
		expectedInfrastructureProvider bool
		expectedTenant                 bool
		verifyChildSpanner             bool
	}{
		{
			desc:                           "GetById success when IPBlock exists",
			id:                             ipb.ID,
			ipb:                            ipb,
			paramRelations:                 []string{},
			expectedError:                  false,
			expectedSite:                   false,
			expectedInfrastructureProvider: false,
			expectedTenant:                 false,
			verifyChildSpanner:             true,
		},
		{
			desc:                           "GetById error when not found",
			id:                             uuid.New(),
			ipb:                            ipb,
			paramRelations:                 []string{},
			expectedError:                  true,
			expectedErrVal:                 db.ErrDoesNotExist,
			expectedSite:                   false,
			expectedInfrastructureProvider: false,
			expectedTenant:                 false,
		},
		{
			desc:                           "GetById with the site relation",
			id:                             ipb.ID,
			ipb:                            ipb,
			paramRelations:                 []string{"Site"},
			expectedError:                  false,
			expectedSite:                   true,
			expectedInfrastructureProvider: false,
			expectedTenant:                 false,
		},
		{
			desc:                           "GetById with site, infrastructure_provider relations",
			id:                             ipb.ID,
			ipb:                            ipb,
			paramRelations:                 []string{"Site", "InfrastructureProvider"},
			expectedError:                  false,
			expectedSite:                   true,
			expectedInfrastructureProvider: true,
			expectedTenant:                 false,
		},
		{
			desc:                           "GetById with site, infrastructure_provider and tenant relations",
			id:                             ipb.ID,
			ipb:                            ipb,
			paramRelations:                 []string{"Site", "InfrastructureProvider", "Tenant"},
			expectedError:                  false,
			expectedSite:                   true,
			expectedInfrastructureProvider: true,
			expectedTenant:                 true,
		},
		{
			desc:                           "GetById with site, infrastructure_provider and tenant relations when tenant is nil",
			id:                             ipb2.ID,
			ipb:                            ipb2,
			paramRelations:                 []string{"Site", "InfrastructureProvider", "Tenant"},
			expectedError:                  false,
			expectedSite:                   true,
			expectedInfrastructureProvider: true,
			expectedTenant:                 false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := ipsd.GetByID(ctx, nil, tc.id, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.EqualValues(t, tc.ipb.ID, tmp.ID)
				assert.Equal(t, tc.expectedSite, tmp.Site != nil)
				if tc.expectedSite {
					assert.EqualValues(t, tc.ipb.SiteID, tmp.Site.ID)
				}
				assert.Equal(t, tc.expectedInfrastructureProvider, tmp.InfrastructureProvider != nil)
				if tc.expectedInfrastructureProvider {
					assert.EqualValues(t, tc.ipb.InfrastructureProviderID, tmp.InfrastructureProvider.ID)
				}
				assert.Equal(t, tc.expectedTenant, tmp.Tenant != nil)
				if tc.expectedTenant {
					assert.EqualValues(t, *tc.ipb.TenantID, tmp.Tenant.ID)
				} else {
					assert.Nil(t, tmp.Tenant)
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

func TestIPBlockSQLDAO_GetOne(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()
	testIPBlockSetupSchema(t, dbSession)
	provider := testIPBlockBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testIPBlockBuildSite(t, dbSession, provider, "testSite")
	tenant := testIPBlockBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	dao := NewIPBlockDAO(dbSession)

	create := func(name string, tenantID, sitePrefixID *uuid.UUID, sequence int) *IPBlock {
		t.Helper()
		ipBlock, err := dao.Create(ctx, nil, IPBlockCreateInput{
			Name:                     name,
			SiteID:                   site.ID,
			InfrastructureProviderID: provider.ID,
			TenantID:                 tenantID,
			SitePrefixID:             sitePrefixID,
			RoutingType:              IPBlockRoutingTypeDatacenterOnly,
			Prefix:                   fmt.Sprintf("10.%d.0.0", sequence),
			PrefixLength:             24,
			ProtocolVersion:          IPBlockProtocolVersionV4,
			Status:                   IPBlockStatusReady,
			CreatedBy:                &user.ID,
		})
		require.NoError(t, err)
		return ipBlock
	}

	// These are the three meaningful identifier combinations: no TenantID is a
	// provider root, TenantID alone is an Allocation, and TenantID with
	// SitePrefixID is a private Tenant SitePrefix.
	providerSitePrefixID := uuid.New()
	providerRoot := create("provider-root", nil, &providerSitePrefixID, 30)
	allocation := create("allocation", &tenant.ID, nil, 31)
	tenantSitePrefixID := uuid.New()
	tenantSitePrefix := create("tenant-site-prefix", &tenant.ID, &tenantSitePrefixID, 32)

	providerVisibleFilter := IPBlockFilterInput{}
	providerVisibleFilter.ProviderVisible(provider.ID)
	siteFabricFilter := IPBlockFilterInput{}
	siteFabricFilter.SiteFabric(provider.ID)
	tenantAllocatedFilter := IPBlockFilterInput{}
	tenantAllocatedFilter.TenantAllocated(tenant.ID)

	tests := []struct {
		name    string
		id      uuid.UUID
		filter  IPBlockFilterInput
		wantErr error
	}{
		{
			name:   "provider sees Site fabric root",
			id:     providerRoot.ID,
			filter: providerVisibleFilter,
		},
		{
			name:   "root filter sees Site fabric root with SitePrefix ID",
			id:     providerRoot.ID,
			filter: siteFabricFilter,
		},
		{
			name:   "provider sees Allocation",
			id:     allocation.ID,
			filter: providerVisibleFilter,
		},
		{
			name:    "provider cannot see Tenant SitePrefix",
			id:      tenantSitePrefix.ID,
			filter:  providerVisibleFilter,
			wantErr: db.ErrDoesNotExist,
		},
		{
			name:   "Allocation filter sees Allocation",
			id:     allocation.ID,
			filter: tenantAllocatedFilter,
		},
		{
			name:    "Allocation filter excludes Tenant SitePrefix",
			id:      tenantSitePrefix.ID,
			filter:  tenantAllocatedFilter,
			wantErr: db.ErrDoesNotExist,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := dao.GetOne(ctx, nil, tt.id, tt.filter, nil)
			if tt.wantErr != nil {
				assert.ErrorIs(t, err, tt.wantErr)
				assert.Nil(t, got)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.id, got.ID)
		})
	}
}

func TestIPBlockSQLDAO_GetCountByStatus(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()
	testIPBlockSetupSchema(t, dbSession)
	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "testIP")
	site1 := testIPBlockBuildSite(t, dbSession, ip, "testSite1")
	tenant := testIPBlockBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	ipsd := NewIPBlockDAO(dbSession)

	ipb, err := ipsd.Create(
		ctx, nil, IPBlockCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("description"),
			SiteID:                   site1.ID,
			InfrastructureProviderID: ip.ID,
			TenantID:                 &tenant.ID,
			RoutingType:              IPBlockRoutingTypePublic,
			Prefix:                   "10.0.1.0",
			PrefixLength:             32,
			ProtocolVersion:          "v4",
			FullGrant:                false,
			Status:                   IPBlockStatusProvisioning,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ipb)

	ipb2, err := ipsd.Create(
		ctx, nil, IPBlockCreateInput{
			Name:                     "test2",
			Description:              cutil.GetPtr("description"),
			SiteID:                   site1.ID,
			InfrastructureProviderID: ip.ID,
			RoutingType:              IPBlockRoutingTypePublic,
			Prefix:                   "10.0.2.0",
			PrefixLength:             32,
			ProtocolVersion:          "v4",
			FullGrant:                false,
			Status:                   IPBlockStatusProvisioning,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ipb2)
	// Provider and Allocation totals exclude this private record. The site-only
	// total includes it, proving the fixture is actually present.
	sitePrefixID := uuid.New()
	_, err = ipsd.Create(ctx, nil, IPBlockCreateInput{
		Name:                     "tenant-site-prefix",
		SiteID:                   site1.ID,
		InfrastructureProviderID: ip.ID,
		TenantID:                 &tenant.ID,
		SitePrefixID:             &sitePrefixID,
		RoutingType:              IPBlockRoutingTypePublic,
		Prefix:                   "10.0.3.0",
		PrefixLength:             32,
		ProtocolVersion:          "v4",
		Status:                   IPBlockStatusProvisioning,
		CreatedBy:                &user.ID,
	})
	require.NoError(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)
	providerVisibleFilter := IPBlockFilterInput{}
	providerVisibleFilter.ProviderVisible(ip.ID)
	tenantAllocatedFilter := IPBlockFilterInput{}
	tenantAllocatedFilter.TenantAllocated(tenant.ID)
	unknownProviderFilter := IPBlockFilterInput{}
	unknownProviderFilter.ProviderVisible(uuid.New())
	statusCounts := func(provisioning int) map[string]int {
		return map[string]int{
			IPBlockStatusDeleting:     0,
			IPBlockStatusError:        0,
			IPBlockStatusReady:        0,
			IPBlockStatusPending:      0,
			IPBlockStatusProvisioning: provisioning,
			"total":                   provisioning,
		}
	}

	tests := []struct {
		name               string
		filter             IPBlockFilterInput
		want               map[string]int
		verifyChildSpanner bool
	}{
		{
			name:               "provider counts exclude Tenant SitePrefix",
			filter:             providerVisibleFilter,
			want:               statusCounts(2),
			verifyChildSpanner: true,
		},
		{
			name:   "site filter counts every record",
			filter: IPBlockFilterInput{SiteIDs: []uuid.UUID{site1.ID}},
			want:   statusCounts(3),
		},
		{
			name:   "Allocation counts exclude Tenant SitePrefix",
			filter: tenantAllocatedFilter,
			want:   statusCounts(1),
		},
		{
			name:   "unknown provider returns zero counts",
			filter: unknownProviderFilter,
			want:   statusCounts(0),
		},
		{
			name: "no filter counts every record",
			want: statusCounts(3),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ipsd.GetCountByStatus(ctx, nil, tt.filter)
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestIPBlockSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()
	testIPBlockSetupSchema(t, dbSession)
	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "testIP")
	site1 := testIPBlockBuildSite(t, dbSession, ip, "testSite1")
	site2 := testIPBlockBuildSite(t, dbSession, ip, "testSite2")
	site3 := testIPBlockBuildSite(t, dbSession, ip, "testSite3")
	tenant := testIPBlockBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	ipbsd := NewIPBlockDAO(dbSession)

	totalCount := 30

	site2ipbs := []IPBlock{}

	for i := 0; i < totalCount; i++ {
		if i%2 == 1 {
			_, err := ipbsd.Create(
				ctx, nil, IPBlockCreateInput{
					Name:                     fmt.Sprintf("test-%v", i),
					Description:              cutil.GetPtr("description"),
					SiteID:                   site1.ID,
					InfrastructureProviderID: ip.ID,
					RoutingType:              IPBlockRoutingTypePublic,
					Prefix:                   fmt.Sprintf("202.16.%v.0", i),
					PrefixLength:             10,
					ProtocolVersion:          "v4",
					FullGrant:                false,
					Status:                   IPBlockStatusProvisioning,
					CreatedBy:                &user.ID,
				},
			)
			assert.NoError(t, err)
		} else {
			ipb, err := ipbsd.Create(
				ctx, nil, IPBlockCreateInput{
					Name:                     fmt.Sprintf("test-%v", i),
					Description:              cutil.GetPtr("description"),
					SiteID:                   site2.ID,
					InfrastructureProviderID: ip.ID,
					TenantID:                 &tenant.ID,
					RoutingType:              IPBlockRoutingTypeDatacenterOnly,
					Prefix:                   fmt.Sprintf("10.0.%v.0", i),
					PrefixLength:             10,
					ProtocolVersion:          "v4",
					FullGrant:                false,
					Status:                   IPBlockStatusProvisioning,
					CreatedBy:                &user.ID,
				},
			)
			assert.NoError(t, err)
			site2ipbs = append(site2ipbs, *ipb)
		}
	}

	deletedIPBlock, err := ipbsd.Create(
		ctx, nil, IPBlockCreateInput{
			Name:                     "deleted-ip-block",
			Description:              cutil.GetPtr("description"),
			SiteID:                   site1.ID,
			InfrastructureProviderID: ip.ID,
			RoutingType:              IPBlockRoutingTypePublic,
			Prefix:                   "203.0.113.0",
			PrefixLength:             24,
			ProtocolVersion:          "v4",
			FullGrant:                false,
			Status:                   IPBlockStatusReady,
			CreatedBy:                &user.ID,
		},
	)
	require.NoError(t, err)
	err = ipbsd.Delete(ctx, nil, deletedIPBlock.ID)
	require.NoError(t, err)

	dummyUUID := uuid.New()

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                      string
		siteIDs                   []uuid.UUID
		infrastructureProviderIDs []uuid.UUID
		tenantIDs                 []uuid.UUID
		routingTypes              []string
		ipbNames                  []string
		fullGrant                 *bool
		excludeDerived            bool
		prefixes                  []string
		prefixLengths             []int
		searchQuery               *string
		statuses                  []string
		ids                       []uuid.UUID
		offset                    *int
		limit                     *int
		orderBy                   *paginator.OrderBy
		firstEntry                *IPBlock
		expectedCount             int
		expectedTotal             *int
		expectedError             bool
		paramRelations            []string
		verifyChildSpanner        bool
		includeDeleted            bool
	}{
		{
			desc:                      "GetAll with no filters returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			expectedCount:             paginator.DefaultLimit,
			expectedTotal:             &totalCount,
			expectedError:             false,
			verifyChildSpanner:        true,
		},
		{
			desc:                      "GetAll with relation returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			expectedCount:             paginator.DefaultLimit,
			expectedTotal:             &totalCount,
			expectedError:             false,
			paramRelations:            []string{"Site", "InfrastructureProvider", "Tenant"},
		},
		{
			desc:                      "GetAll returns no objects",
			siteIDs:                   []uuid.UUID{dummyUUID},
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			expectedCount:             0,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with site filter returns objects",
			siteIDs:                   []uuid.UUID{site1.ID},
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			ipbNames:                  nil,
			routingTypes:              nil,
			expectedCount:             totalCount / 2,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with RoutingTypePublic filter returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              []string{IPBlockRoutingTypePublic},
			ipbNames:                  nil,
			expectedCount:             totalCount / 2,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with RoutingTypeDatacenterOnly filter returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              []string{IPBlockRoutingTypeDatacenterOnly},
			ipbNames:                  nil,
			expectedCount:             totalCount / 2,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with site filter returns no objects",
			siteIDs:                   []uuid.UUID{site3.ID},
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			ipbNames:                  nil,
			routingTypes:              nil,
			expectedCount:             0,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with site and infrastructure provider filter returns objects",
			siteIDs:                   []uuid.UUID{site1.ID},
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			expectedCount:             totalCount / 2,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with site and infrastructure provider filter returns no objects",
			siteIDs:                   []uuid.UUID{site3.ID},
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			expectedCount:             0,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with site infrastructure and tenant filters returns objects",
			siteIDs:                   []uuid.UUID{site2.ID},
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			tenantIDs:                 []uuid.UUID{tenant.ID},
			routingTypes:              nil,
			ipbNames:                  nil,
			expectedCount:             totalCount / 2,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with site infrastructure and exclude derived filters returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			excludeDerived:            true,
			routingTypes:              nil,
			ipbNames:                  nil,
			expectedCount:             totalCount / 2,
			expectedError:             false,
		}, {
			desc:                      "GetAll with site infrastructure and tenant filters returns no objects",
			siteIDs:                   []uuid.UUID{site3.ID},
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			tenantIDs:                 []uuid.UUID{tenant.ID},
			routingTypes:              nil,
			ipbNames:                  nil,
			expectedCount:             0,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with site infrastructure tenant and name filters returns objects",
			siteIDs:                   []uuid.UUID{site2.ID},
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			tenantIDs:                 []uuid.UUID{tenant.ID},
			routingTypes:              nil,
			ipbNames:                  []string{"test-0"},
			fullGrant:                 cutil.GetPtr(false),
			expectedCount:             1,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with limit returns objects",
			siteIDs:                   []uuid.UUID{site1.ID},
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			offset:                    cutil.GetPtr(0),
			limit:                     cutil.GetPtr(5),
			expectedCount:             5,
			expectedTotal:             cutil.GetPtr(totalCount / 2),
			expectedError:             false,
		},
		{
			desc:                      "GetAll with offset returns objects",
			siteIDs:                   []uuid.UUID{site1.ID},
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			offset:                    cutil.GetPtr(5),
			expectedCount:             10,
			expectedTotal:             cutil.GetPtr(totalCount / 2),
			expectedError:             false,
		},
		{
			desc:                      "GetAll with order by returns objects",
			siteIDs:                   []uuid.UUID{site2.ID},
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			orderBy: &paginator.OrderBy{
				Field: "name",
				Order: paginator.OrderDescending,
			},
			firstEntry:    &site2ipbs[4], // 5th entry is "test-8" and would appear first in descending order
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:                      "GetAll with name search query returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			searchQuery:               cutil.GetPtr("test-"),
			expectedCount:             paginator.DefaultLimit,
			expectedTotal:             &totalCount,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with description  search query returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			searchQuery:               cutil.GetPtr("description"),
			expectedCount:             paginator.DefaultLimit,
			expectedTotal:             &totalCount,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with status search query returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			searchQuery:               cutil.GetPtr(IPBlockStatusProvisioning),
			expectedCount:             paginator.DefaultLimit,
			expectedTotal:             &totalCount,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with empty search query returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			searchQuery:               cutil.GetPtr(""),
			expectedCount:             paginator.DefaultLimit,
			expectedTotal:             &totalCount,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with IPBlockStatusProvisioning status returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			statuses:                  []string{IPBlockStatusProvisioning},
			expectedCount:             paginator.DefaultLimit,
			expectedTotal:             &totalCount,
			expectedError:             false,
		},
		{
			desc:                      "GetAll with ids filters returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			ids: []uuid.UUID{
				site2ipbs[0].ID,
				site2ipbs[1].ID,
				site2ipbs[2].ID,
			},
			expectedCount: 3,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc:                      "GetAll with status and ids filters returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			ids: []uuid.UUID{
				site2ipbs[0].ID,
				site2ipbs[1].ID,
				site2ipbs[2].ID,
			},
			statuses:      []string{IPBlockStatusProvisioning},
			expectedCount: 3,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc:                      "GetAll with status and ids filters returns no objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			ids: []uuid.UUID{
				site2ipbs[0].ID,
				site2ipbs[1].ID,
				site2ipbs[2].ID,
			},
			statuses:      []string{IPBlockStatusError},
			expectedCount: 0,
			expectedTotal: cutil.GetPtr(0),
			expectedError: false,
		},
		{
			desc:                      "GetAll with IPBlockStatusError status returns objects",
			siteIDs:                   nil,
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			statuses:                  []string{IPBlockStatusError},
			expectedCount:             0,
			expectedTotal:             cutil.GetPtr(0),
			expectedError:             false,
		},
		{
			desc:          "GetAll excludes a soft-deleted IP Block by default",
			ids:           []uuid.UUID{deletedIPBlock.ID},
			expectedCount: 0,
			expectedTotal: cutil.GetPtr(0),
			expectedError: false,
		},
		{
			desc:           "GetAll includes a soft-deleted IP Block when requested",
			ids:            []uuid.UUID{deletedIPBlock.ID},
			includeDeleted: true,
			expectedCount:  1,
			expectedTotal:  cutil.GetPtr(1),
			expectedError:  false,
			firstEntry:     deletedIPBlock,
		},
		{
			desc:                      "GetAll with site, prefix and prefixLenth returns object",
			siteIDs:                   []uuid.UUID{site2.ID},
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			prefixes:                  []string{"10.0.2.0"},
			prefixLengths:             []int{10},
			statuses:                  nil,
			expectedCount:             1,
			expectedTotal:             cutil.GetPtr(1),
			expectedError:             false,
		},
		{
			desc:                      "GetAll with site, prefix and prefixLenth returns no object",
			siteIDs:                   []uuid.UUID{site1.ID},
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			tenantIDs:                 nil,
			routingTypes:              nil,
			ipbNames:                  nil,
			prefixes:                  []string{"10.0.2.0"},
			prefixLengths:             []int{10},
			statuses:                  nil,
			expectedCount:             0,
			expectedTotal:             cutil.GetPtr(0),
			expectedError:             false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := ipbsd.GetAll(
				ctx,
				nil,
				IPBlockFilterInput{
					SiteIDs:                   tc.siteIDs,
					InfrastructureProviderIDs: tc.infrastructureProviderIDs,
					TenantIDs:                 tc.tenantIDs,
					RoutingTypes:              tc.routingTypes,
					Names:                     tc.ipbNames,
					FullGrant:                 tc.fullGrant,
					ExcludeDerived:            tc.excludeDerived,
					Prefixes:                  tc.prefixes,
					PrefixLengths:             tc.prefixLengths,
					Statuses:                  tc.statuses,
					SearchQuery:               tc.searchQuery,
					IPBlockIDs:                tc.ids,
					IncludeDeleted:            tc.includeDeleted,
				},
				paginator.PageInput{
					Offset:  tc.offset,
					Limit:   tc.limit,
					OrderBy: tc.orderBy,
				},
				tc.paramRelations,
			)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
				assert.Equal(t, tc.expectedCount, len(got))
				if len(tc.paramRelations) > 0 {
					assert.NotNil(t, got[0].Site)
					assert.NotNil(t, got[0].Tenant)
					assert.NotNil(t, got[0].InfrastructureProvider)
				}
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.firstEntry != nil {
				assert.Equal(t, tc.firstEntry.Name, got[0].Name)
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

func TestIPBlockSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()
	testIPBlockSetupSchema(t, dbSession)
	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "testIP")
	ip2 := testIPBlockBuildInfrastructureProvider(t, dbSession, "testIP2")
	site1 := testIPBlockBuildSite(t, dbSession, ip, "testSite1")
	site2 := testIPBlockBuildSite(t, dbSession, ip, "testSite2")
	tenant := testIPBlockBuildTenant(t, dbSession, "testTenant")
	tenant2 := testIPBlockBuildTenant(t, dbSession, "testTenant2")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	ipbsd := NewIPBlockDAO(dbSession)

	ipb, err := ipbsd.Create(
		ctx, nil, IPBlockCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("description"),
			SiteID:                   site1.ID,
			InfrastructureProviderID: ip.ID,
			TenantID:                 &tenant.ID,
			RoutingType:              IPBlockRoutingTypePublic,
			Prefix:                   "10.0.1.0",
			PrefixLength:             32,
			ProtocolVersion:          "v4",
			FullGrant:                false,
			Status:                   IPBlockStatusProvisioning,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ipb)

	newBlockSize := 128

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc string
		ipb  *IPBlock

		paramName                     *string
		paramDescription              *string
		paramSiteID                   *uuid.UUID
		paramInfrastructureProviderID *uuid.UUID
		paramTenantID                 *uuid.UUID
		paramRoutingType              *string
		paramPrefix                   *string
		paramBlockSize                *int
		paramProtocolVersion          *string
		paramFullGrant                *bool
		paramStatus                   *string

		expectedName                     *string
		expectedDescription              *string
		expectedSiteID                   *uuid.UUID
		expectedInfrastructureProviderID *uuid.UUID
		expectedTenantID                 *uuid.UUID
		expectedRoutingType              *string
		expectedPrefix                   *string
		expectedBlockSize                *int
		expectedProtocolVersion          *string
		expectedFullGrant                *bool
		expectedStatus                   *string
		verifyChildSpanner               bool
	}{
		{
			desc: "can update string, bool fields: name, description, routingtype, prefix, protocolversion, status",
			ipb:  ipb,

			paramName:                     cutil.GetPtr("updatedName"),
			paramDescription:              cutil.GetPtr("updatedDescription"),
			paramSiteID:                   nil,
			paramInfrastructureProviderID: nil,
			paramTenantID:                 nil,
			paramRoutingType:              cutil.GetPtr("updatedRoutingType"),
			paramPrefix:                   cutil.GetPtr("updatedPrefix"),
			paramBlockSize:                nil,
			paramProtocolVersion:          cutil.GetPtr("updatedProtocolVersion"),
			paramFullGrant:                cutil.GetPtr(true),
			paramStatus:                   cutil.GetPtr(IPBlockStatusProvisioning),

			expectedName:                     cutil.GetPtr("updatedName"),
			expectedDescription:              cutil.GetPtr("updatedDescription"),
			expectedSiteID:                   &ipb.SiteID,
			expectedInfrastructureProviderID: &ipb.InfrastructureProviderID,
			expectedTenantID:                 ipb.TenantID,
			expectedRoutingType:              cutil.GetPtr("updatedRoutingType"),
			expectedPrefix:                   cutil.GetPtr("updatedPrefix"),
			expectedBlockSize:                &ipb.PrefixLength,
			expectedProtocolVersion:          cutil.GetPtr("updatedProtocolVersion"),
			expectedFullGrant:                cutil.GetPtr(true),
			expectedStatus:                   cutil.GetPtr(IPBlockStatusProvisioning),
			verifyChildSpanner:               true,
		},
		{
			desc: "can update uuid fields: siteid, infrastructureproviderid, tenantid",
			ipb:  ipb,

			paramName:                     nil,
			paramDescription:              nil,
			paramSiteID:                   &site2.ID,
			paramInfrastructureProviderID: &ip2.ID,
			paramTenantID:                 &tenant2.ID,
			paramRoutingType:              nil,
			paramPrefix:                   nil,
			paramBlockSize:                nil,
			paramProtocolVersion:          nil,
			paramStatus:                   nil,

			expectedName:                     cutil.GetPtr("updatedName"),
			expectedDescription:              cutil.GetPtr("updatedDescription"),
			expectedSiteID:                   &site2.ID,
			expectedInfrastructureProviderID: &ip2.ID,
			expectedTenantID:                 &tenant2.ID,
			expectedRoutingType:              cutil.GetPtr("updatedRoutingType"),
			expectedPrefix:                   cutil.GetPtr("updatedPrefix"),
			expectedBlockSize:                &ipb.PrefixLength,
			expectedProtocolVersion:          cutil.GetPtr("updatedProtocolVersion"),
			expectedStatus:                   cutil.GetPtr(IPBlockStatusProvisioning),
		},
		{
			desc: "can update int fields: blocksize",
			ipb:  ipb,

			paramName:                     nil,
			paramDescription:              nil,
			paramSiteID:                   nil,
			paramInfrastructureProviderID: nil,
			paramTenantID:                 nil,
			paramRoutingType:              nil,
			paramPrefix:                   nil,
			paramBlockSize:                &newBlockSize,
			paramProtocolVersion:          nil,
			paramStatus:                   nil,

			expectedName:                     cutil.GetPtr("updatedName"),
			expectedDescription:              cutil.GetPtr("updatedDescription"),
			expectedSiteID:                   &site2.ID,
			expectedInfrastructureProviderID: &ip2.ID,
			expectedTenantID:                 &tenant2.ID,
			expectedRoutingType:              cutil.GetPtr("updatedRoutingType"),
			expectedPrefix:                   cutil.GetPtr("updatedPrefix"),
			expectedBlockSize:                &newBlockSize,
			expectedProtocolVersion:          cutil.GetPtr("updatedProtocolVersion"),
			expectedStatus:                   cutil.GetPtr(IPBlockStatusProvisioning),
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := ipbsd.Update(
				ctx,
				nil,
				IPBlockUpdateInput{
					IPBlockID:                tc.ipb.ID,
					Name:                     tc.paramName,
					Description:              tc.paramDescription,
					SiteID:                   tc.paramSiteID,
					InfrastructureProviderID: tc.paramInfrastructureProviderID,
					TenantID:                 tc.paramTenantID,
					RoutingType:              tc.paramRoutingType,
					Prefix:                   tc.paramPrefix,
					PrefixLength:             tc.paramBlockSize,
					ProtocolVersion:          tc.paramProtocolVersion,
					FullGrant:                tc.paramFullGrant,
					Status:                   tc.paramStatus,
				},
			)
			assert.Nil(t, err)
			assert.NotNil(t, got)

			assert.Equal(t, *tc.expectedName, got.Name)

			assert.Equal(t, tc.expectedDescription == nil, got.Description == nil)
			if tc.expectedDescription != nil {
				assert.Equal(t, *tc.expectedDescription, *got.Description)
			}
			assert.Equal(t, *tc.expectedSiteID, got.SiteID)
			assert.Equal(t, *tc.expectedInfrastructureProviderID, got.InfrastructureProviderID)
			assert.Equal(t, tc.expectedTenantID == nil, got.TenantID == nil)
			if tc.expectedTenantID != nil {
				assert.Equal(t, *tc.expectedTenantID, *got.TenantID)
			}
			assert.Equal(t, *tc.expectedRoutingType, got.RoutingType)
			assert.Equal(t, *tc.expectedPrefix, got.Prefix)
			assert.Equal(t, *tc.expectedBlockSize, got.PrefixLength)
			assert.Equal(t, *tc.expectedProtocolVersion, got.ProtocolVersion)
			assert.Equal(t, *tc.expectedStatus, got.Status)

			if tc.paramFullGrant != nil {
				assert.Equal(t, *tc.expectedFullGrant, got.FullGrant)
			}

			if got.Updated.String() == tc.ipb.Updated.String() {
				t.Errorf("got.Updated = %v, want different value", got.Updated)
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

func TestIPBlockSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()
	testIPBlockSetupSchema(t, dbSession)
	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "testIP")
	site1 := testIPBlockBuildSite(t, dbSession, ip, "testSite1")
	site2 := testIPBlockBuildSite(t, dbSession, ip, "testSite2")
	tenant := testIPBlockBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	ipbsd := NewIPBlockDAO(dbSession)

	ipb, err := ipbsd.Create(
		ctx, nil, IPBlockCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("description"),
			SiteID:                   site1.ID,
			InfrastructureProviderID: ip.ID,
			TenantID:                 &tenant.ID,
			RoutingType:              IPBlockRoutingTypePublic,
			Prefix:                   "10.0.1.0",
			PrefixLength:             32,
			ProtocolVersion:          "v4",
			FullGrant:                false,
			Status:                   IPBlockStatusProvisioning,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ipb)

	ipb2, err := ipbsd.Create(
		ctx, nil, IPBlockCreateInput{
			Name:                     "test2",
			Description:              cutil.GetPtr("description"),
			SiteID:                   site1.ID,
			InfrastructureProviderID: ip.ID,
			TenantID:                 &tenant.ID,
			RoutingType:              IPBlockRoutingTypePublic,
			Prefix:                   "10.0.2.0",
			PrefixLength:             32,
			ProtocolVersion:          "v4",
			FullGrant:                false,
			Status:                   IPBlockStatusProvisioning,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ipb2)

	ipb3, err := ipbsd.Create(
		ctx, nil, IPBlockCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("description"),
			SiteID:                   site2.ID,
			InfrastructureProviderID: ip.ID,
			TenantID:                 &tenant.ID,
			RoutingType:              IPBlockRoutingTypePublic,
			Prefix:                   "10.0.3.0",
			PrefixLength:             32,
			ProtocolVersion:          "v4",
			FullGrant:                false,
			Status:                   IPBlockStatusProvisioning,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ipb3)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                string
		ipb                 *IPBlock
		paramDescription    bool
		paramTenantID       bool
		expectedUpdate      bool
		expectedDescription *string
		expectedTenantID    *uuid.UUID
		expectedError       bool
		verifyChildSpanner  bool
	}{
		{
			desc:                "can clear description",
			ipb:                 ipb,
			paramDescription:    true,
			paramTenantID:       false,
			expectedUpdate:      true,
			expectedDescription: nil,
			expectedTenantID:    ipb.TenantID,
			expectedError:       false,
			verifyChildSpanner:  true,
		},
		{
			desc:                "can clear tenant",
			ipb:                 ipb2,
			paramDescription:    false,
			paramTenantID:       true,
			expectedUpdate:      true,
			expectedDescription: cutil.GetPtr("description"),
			expectedTenantID:    nil,
			expectedError:       false,
		},
		{
			desc:                "can clear multiple fields at once",
			ipb:                 ipb3,
			paramDescription:    true,
			paramTenantID:       true,
			expectedUpdate:      true,
			expectedDescription: nil,
			expectedTenantID:    nil,
			expectedError:       false,
		},
		{
			desc:                "nop when no cleared fields are specified",
			ipb:                 ipb,
			paramDescription:    false,
			paramTenantID:       false,
			expectedUpdate:      false,
			expectedDescription: nil,
			expectedTenantID:    ipb.TenantID,
			expectedError:       false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := ipbsd.Clear(ctx, nil, IPBlockClearInput{
				IPBlockID:   tc.ipb.ID,
				Description: tc.paramDescription,
				TenantID:    tc.paramTenantID,
			})
			assert.Equal(t, tc.expectedError, err != nil)
			assert.NotNil(t, tmp)
			assert.Equal(t, tc.expectedDescription == nil, tmp.Description == nil)
			if tc.expectedDescription != nil {
				assert.Equal(t, *tc.expectedDescription, *tmp.Description)
			}
			assert.Equal(t, tc.expectedTenantID == nil, tmp.TenantID == nil)
			if tc.expectedTenantID != nil {
				assert.Equal(t, *tc.expectedTenantID, *tmp.TenantID)
			}

			if tc.expectedUpdate {
				assert.True(t, tmp.Updated.After(tc.ipb.Updated))
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

func TestIPBlockSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()
	testIPBlockSetupSchema(t, dbSession)
	ip := testIPBlockBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testIPBlockBuildSite(t, dbSession, ip, "testSite")
	tenant := testIPBlockBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	ipbsd := NewIPBlockDAO(dbSession)

	ipb, err := ipbsd.Create(
		ctx, nil, IPBlockCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("description"),
			SiteID:                   site.ID,
			InfrastructureProviderID: ip.ID,
			TenantID:                 &tenant.ID,
			RoutingType:              IPBlockRoutingTypePublic,
			Prefix:                   "10.0.1.0",
			PrefixLength:             32,
			ProtocolVersion:          "v4",
			FullGrant:                false,
			Status:                   IPBlockStatusProvisioning,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ipb)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		ipbID              uuid.UUID
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object",
			ipbID:              ipb.ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "delete non-existing object",
			ipbID:         uuid.New(),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := ipbsd.Delete(ctx, nil, tc.ipbID)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := ipbsd.GetByID(ctx, nil, tc.ipbID, nil)
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

type testIPBlockAfterUpdateHook struct {
	afterUpdate func()
}

func (h *testIPBlockAfterUpdateHook) BeforeQuery(ctx context.Context, _ *bun.QueryEvent) context.Context {
	return ctx
}

func (h *testIPBlockAfterUpdateHook) AfterQuery(_ context.Context, event *bun.QueryEvent) {
	if h.afterUpdate == nil || event.Err != nil || event.Operation() != "UPDATE" {
		return
	}
	afterUpdate := h.afterUpdate
	h.afterUpdate = nil
	afterUpdate()
}

func TestIPBlockSQLDAO_LinkSitePrefix(t *testing.T) {
	ctx := context.Background()
	dbSession := testIPBlockInitDB(t)
	defer dbSession.Close()
	testIPBlockSetupSchema(t, dbSession)
	provider := testIPBlockBuildInfrastructureProvider(t, dbSession, "link-site-prefix")
	site := testIPBlockBuildSite(t, dbSession, provider, "link-site-prefix")
	user := testInstanceBuildUser(t, dbSession, "link-site-prefix")
	dao := NewIPBlockDAO(dbSession)

	sequence := 0
	create := func(name string) *IPBlock {
		t.Helper()
		sequence++
		ipBlock, err := dao.Create(ctx, nil, IPBlockCreateInput{
			Name:                     name,
			SiteID:                   site.ID,
			InfrastructureProviderID: provider.ID,
			RoutingType:              IPBlockRoutingTypeDatacenterOnly,
			Prefix:                   fmt.Sprintf("10.90.%d.0", sequence),
			PrefixLength:             24,
			ProtocolVersion:          IPBlockProtocolVersionV4,
			Status:                   IPBlockStatusReady,
			CreatedBy:                &user.ID,
		})
		require.NoError(t, err)
		return ipBlock
	}

	tests := []struct {
		name                   string
		linkFirst              bool
		requestDifferent       bool
		deleteBeforeLink       bool
		deleteAfterUpdate      bool
		expectedError          error
		expectLink             bool
		expectUpdatedUnchanged bool
	}{
		{
			name:       "links an active unlinked IP Block",
			expectLink: true,
		},
		{
			name:              "returns the linked snapshot when deletion follows the update",
			deleteAfterUpdate: true,
			expectLink:        true,
		},
		{
			name:                   "repeats the same link",
			linkFirst:              true,
			expectLink:             true,
			expectUpdatedUnchanged: true,
		},
		{
			name:             "rejects reassignment to another SitePrefix",
			linkFirst:        true,
			requestDifferent: true,
			expectedError:    db.ErrInvalidValue,
			expectLink:       true,
		},
		{
			name:             "rejects a deleted IP Block",
			deleteBeforeLink: true,
			expectedError:    db.ErrInvalidValue,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			ipBlock := create(tc.name)
			sitePrefixID := uuid.New()
			fixedUpdated := time.Date(2020, time.January, 2, 3, 4, 5, 0, time.UTC)
			if tc.linkFirst {
				_, err := dao.LinkSitePrefix(ctx, nil, ipBlock.ID, sitePrefixID)
				require.NoError(t, err)
			}
			if tc.expectUpdatedUnchanged {
				_, err := dbSession.DB.NewUpdate().
					Model((*IPBlock)(nil)).
					Set("updated = ?", fixedUpdated).
					Where("id = ?", ipBlock.ID).
					Exec(ctx)
				require.NoError(t, err)
			}
			if tc.deleteBeforeLink {
				require.NoError(t, dao.Delete(ctx, nil, ipBlock.ID))
			}

			requestedID := sitePrefixID
			if tc.requestDifferent {
				requestedID = uuid.New()
			}
			var deleteErr error
			deleteRan := false
			if tc.deleteAfterUpdate {
				hook := &testIPBlockAfterUpdateHook{
					afterUpdate: func() {
						deleteRan = true
						deleteErr = dao.Delete(ctx, nil, ipBlock.ID)
					},
				}
				dbSession.DB.AddQueryHook(hook)
				t.Cleanup(func() { hook.afterUpdate = nil })
			}
			got, err := dao.LinkSitePrefix(ctx, nil, ipBlock.ID, requestedID)
			if tc.deleteAfterUpdate {
				require.True(t, deleteRan)
				require.NoError(t, deleteErr)
			}
			if tc.expectedError != nil {
				require.ErrorIs(t, err, tc.expectedError)
				if tc.expectLink {
					got, err = dao.GetByID(ctx, nil, ipBlock.ID, nil)
					require.NoError(t, err)
				}
			} else {
				require.NoError(t, err)
			}
			if tc.expectLink {
				require.NotNil(t, got)
				require.NotNil(t, got.SitePrefixID)
				assert.Equal(t, sitePrefixID, *got.SitePrefixID)
			}
			if tc.deleteAfterUpdate {
				assert.Nil(t, got.Deleted)
				var stored IPBlock
				err = dbSession.DB.NewSelect().Model(&stored).WhereAllWithDeleted().Where("id = ?", ipBlock.ID).Scan(ctx)
				require.NoError(t, err)
				require.NotNil(t, stored.SitePrefixID)
				assert.Equal(t, sitePrefixID, *stored.SitePrefixID)
				assert.NotNil(t, stored.Deleted)
			}
			if tc.expectUpdatedUnchanged {
				assert.True(t, got.Updated.Equal(fixedUpdated))
			}
		})
	}
}
