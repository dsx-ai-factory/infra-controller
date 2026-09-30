// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"

	"github.com/google/uuid"
	"github.com/uptrace/bun/extra/bundebug"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestVpcPrefix_ToProto(t *testing.T) {
	prefixID := uuid.New()
	vpcID := uuid.New()
	vp := &VpcPrefix{ID: prefixID, Name: "prefix-a", Prefix: "10.0.0.0/16"}
	vpc := &Vpc{ID: vpcID}

	t.Run("populates wire fields and translates VpcId via the parent VPC", func(t *testing.T) {
		proto := vp.ToProto(vpc)
		require.NotNil(t, proto)
		require.NotNil(t, proto.Id)
		assert.Equal(t, prefixID.String(), proto.Id.Value)
		require.NotNil(t, proto.VpcId)
		assert.Equal(t, vpcID.String(), proto.VpcId.Value)
		require.NotNil(t, proto.Config)
		assert.Equal(t, "10.0.0.0/16", proto.Config.Prefix)
		require.NotNil(t, proto.Metadata)
		assert.Equal(t, "prefix-a", proto.Metadata.Name)
	})

	t.Run("leaves VpcId nil when parent VPC is not provided", func(t *testing.T) {
		proto := vp.ToProto(nil)
		require.NotNil(t, proto)
		assert.Nil(t, proto.VpcId)
		require.NotNil(t, proto.Id)
		assert.Equal(t, prefixID.String(), proto.Id.Value)
	})
}

// TestVpcPrefix_GetCIDR verifies stored IPv4 and IPv6 prefixes parse into the
// netip.Prefix used by the usage calculation.
func TestVpcPrefix_GetCIDR(t *testing.T) {
	tests := []struct {
		name      string
		prefix    string
		prefixLen int
		want      string
		wantErr   bool
	}{
		{
			name: "unset prefix",
		},
		{
			name:      "IPv4 address uses the stored prefix length",
			prefix:    "192.0.2.0",
			prefixLen: 24,
			want:      "192.0.2.0/24",
		},
		{
			name:   "IPv4 CIDR is masked to its network",
			prefix: "192.0.2.1/24",
			want:   "192.0.2.0/24",
		},
		{
			name:   "IPv6 CIDR is preserved",
			prefix: "2001:db8::/64",
			want:   "2001:db8::/64",
		},
		{
			name:    "malformed CIDR returns an error",
			prefix:  "not-a-prefix",
			wantErr: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			vp := VpcPrefix{Prefix: tc.prefix, PrefixLength: tc.prefixLen}
			got, err := vp.GetCIDR()
			if tc.wantErr {
				require.Error(t, err)
				assert.False(t, got.IsValid())
				return
			}
			require.NoError(t, err)
			if tc.want == "" {
				assert.False(t, got.IsValid())
				return
			}
			require.True(t, got.IsValid())
			assert.Equal(t, tc.want, got.String())
		})
	}
}

func TestVpcPrefix_FromProto(t *testing.T) {
	t.Run("nil proto is a no-op", func(t *testing.T) {
		original := VpcPrefix{ID: uuid.New(), Name: "original", Prefix: "10.0.0.0/16"}
		vp := original
		vp.FromProto(nil)
		assert.Equal(t, original, vp)
	})

	t.Run("populates fields from proto Metadata and Config", func(t *testing.T) {
		prefixID := uuid.New()
		vpcID := uuid.New()
		vp := &VpcPrefix{}
		vp.FromProto(&corev1.VpcPrefix{
			Id:       &corev1.VpcPrefixId{Value: prefixID.String()},
			VpcId:    &corev1.VpcId{Value: vpcID.String()},
			Config:   &corev1.VpcPrefixConfig{Prefix: "10.0.0.0/16"},
			Metadata: &corev1.Metadata{Name: "prefix-a"},
		})
		assert.Equal(t, prefixID, vp.ID)
		assert.Equal(t, vpcID, vp.VpcID)
		assert.Equal(t, "prefix-a", vp.Name)
		assert.Equal(t, "10.0.0.0/16", vp.Prefix)
	})

	t.Run("falls back to deprecated top-level Name and Prefix", func(t *testing.T) {
		prefixID := uuid.New()
		vp := &VpcPrefix{}
		vp.FromProto(&corev1.VpcPrefix{
			Id:     &corev1.VpcPrefixId{Value: prefixID.String()},
			Name:   "prefix-legacy",
			Prefix: "10.1.0.0/16",
		})
		assert.Equal(t, "prefix-legacy", vp.Name)
		assert.Equal(t, "10.1.0.0/16", vp.Prefix)
	})

	t.Run("preserves ID when proto Id is unparseable", func(t *testing.T) {
		preserved := uuid.New()
		vp := &VpcPrefix{ID: preserved}
		vp.FromProto(&corev1.VpcPrefix{
			Id: &corev1.VpcPrefixId{Value: "not-a-uuid"},
		})
		assert.Equal(t, preserved, vp.ID)
	})

	t.Run("clears VpcID when proto omits VpcId", func(t *testing.T) {
		vp := &VpcPrefix{VpcID: uuid.New()}
		vp.FromProto(&corev1.VpcPrefix{})
		assert.Equal(t, uuid.Nil, vp.VpcID)
	})

	t.Run("clears VpcID when proto VpcId is unparseable", func(t *testing.T) {
		vp := &VpcPrefix{VpcID: uuid.New()}
		vp.FromProto(&corev1.VpcPrefix{
			VpcId: &corev1.VpcId{Value: "not-a-uuid"},
		})
		assert.Equal(t, uuid.Nil, vp.VpcID)
	})
}

func TestVpcPrefix_ToDeletionRequestProto(t *testing.T) {
	prefixID := uuid.New()
	vp := &VpcPrefix{ID: prefixID}
	req := vp.ToDeletionRequestProto()
	require.NotNil(t, req)
	require.NotNil(t, req.Id)
	assert.Equal(t, prefixID.String(), req.Id.Value)
}

func testVpcPrefixInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// reset the tables needed for VpcPrefix tests
func testVpcPrefixSetupSchema(t *testing.T, dbSession *db.Session) {
	// create Vpc table
	err := dbSession.DB.ResetModel(context.Background(), (*Vpc)(nil))
	assert.Nil(t, err)
	// create VpcPrefix table
	err = dbSession.DB.ResetModel(context.Background(), (*VpcPrefix)(nil))
	assert.Nil(t, err)
	// create domain table
	err = dbSession.DB.ResetModel(context.Background(), (*Domain)(nil))
	assert.Nil(t, err)
	// create tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	assert.Nil(t, err)
	// create ipblock table
	err = dbSession.DB.ResetModel(context.Background(), (*IPBlock)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
}

func testVpcPrefixBuildTenant(t *testing.T, dbSession *db.Session, name string) *Tenant {
	tenant := &Tenant{
		ID:   uuid.New(),
		Name: name,
		Org:  "test",
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

func testVpcPrefixBuildInfrastructureProvider(t *testing.T, dbSession *db.Session, name string) *InfrastructureProvider {
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

func testVpcPrefixBuildSite(t *testing.T, dbSession *db.Session, ip *InfrastructureProvider, name string) *Site {
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

func testVpcPrefixBuildVpc(t *testing.T, dbSession *db.Session, infrastructureProvider *InfrastructureProvider, site *Site, tenant *Tenant, name string) *Vpc {
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

func testVpcPrefixBuildDomain(t *testing.T, dbSession *db.Session, hostname string) *Domain {
	domain := &Domain{
		ID:       uuid.New(),
		Hostname: hostname,
		Org:      "test",
	}
	_, err := dbSession.DB.NewInsert().Model(domain).Exec(context.Background())
	assert.Nil(t, err)
	return domain
}

func testVpcPrefixBuildIPBlock(t *testing.T, dbSession *db.Session, siteID, infrastructureProviderID *uuid.UUID, name string, createdBy *uuid.UUID) *IPBlock {
	ipBlock := &IPBlock{
		ID:                       uuid.New(),
		Name:                     name,
		SiteID:                   *siteID,
		InfrastructureProviderID: *infrastructureProviderID,
		PrefixLength:             8,
		RoutingType:              IPBlockRoutingTypeDatacenterOnly,
		CreatedBy:                createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(ipBlock).Exec(context.Background())
	assert.Nil(t, err)
	return ipBlock
}

func testVpcPrefixBuildUser(t *testing.T, dbSession *db.Session, starfleetID string) *User {
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

func TestVpcPrefixSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVpcPrefixSetupSchema(t, dbSession)
	ip := testVpcPrefixBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testVpcPrefixBuildSite(t, dbSession, ip, "testSite")
	tenant := testVpcPrefixBuildTenant(t, dbSession, "testTenant")
	user := testVpcPrefixBuildUser(t, dbSession, "testUser")
	ipBlock := testVpcPrefixBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipBlock", &user.ID)
	vpc := testVpcPrefixBuildVpc(t, dbSession, ip, site, tenant, "testVpc")

	vpd := NewVpcPrefixDAO(dbSession)

	prefix := "192.0.2.0/24"

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		vps                []VpcPrefix
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			vps: []VpcPrefix{
				{
					Name: "test", Org: "test", SiteID: site.ID, VpcID: vpc.ID, TenantID: tenant.ID, IPBlockID: &ipBlock.ID, Prefix: prefix, PrefixLength: 24, Status: VpcPrefixStatusReady, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create mulitiple, some with null values in nullable fields",
			vps: []VpcPrefix{
				{
					Name: "test1", Org: "test", SiteID: site.ID, VpcID: vpc.ID, TenantID: tenant.ID, IPBlockID: &ipBlock.ID, Prefix: prefix, PrefixLength: 24, Status: VpcPrefixStatusReady, CreatedBy: user.ID,
				},
				{
					Name: "test2", Org: "test", SiteID: site.ID, VpcID: vpc.ID, TenantID: tenant.ID, IPBlockID: &ipBlock.ID, Prefix: prefix, PrefixLength: 24, Status: VpcPrefixStatusReady, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create fails, due to foreign key violation on vpcID",
			vps: []VpcPrefix{
				{
					Name: "test", Org: "test", SiteID: site.ID, VpcID: uuid.New(), TenantID: tenant.ID, IPBlockID: &ipBlock.ID, Prefix: prefix, PrefixLength: 24, Status: VpcPrefixStatusReady, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.vps {
				got, err := vpd.Create(ctx, nil, VpcPrefixCreateInput{
					Name:         i.Name,
					TenantOrg:    i.Org,
					SiteID:       i.SiteID,
					VpcID:        i.VpcID,
					TenantID:     i.TenantID,
					IpBlockID:    i.IPBlockID,
					Prefix:       i.Prefix,
					PrefixLength: i.PrefixLength,
					Status:       i.Status,
					CreatedBy:    i.CreatedBy,
				})
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, got)
					assert.Equal(t, i.Prefix, got.Prefix)
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

func TestVpcPrefixSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVpcPrefixSetupSchema(t, dbSession)

	ip := testVpcPrefixBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testVpcPrefixBuildSite(t, dbSession, ip, "testSite")
	tenant := testVpcPrefixBuildTenant(t, dbSession, "testTenant")
	user := testVpcPrefixBuildUser(t, dbSession, "testUser")
	ipBlock := testVpcPrefixBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipBlock", &user.ID)
	vpc := testVpcPrefixBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	vps := NewVpcPrefixDAO(dbSession)

	prefix := "192.0.2.0/24"

	vpcprefix, err := vps.Create(ctx, nil, VpcPrefixCreateInput{
		Name:         "test",
		TenantOrg:    "test",
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipBlock.ID,
		Prefix:       prefix,
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    user.ID,
	})

	assert.Nil(t, err)
	assert.NotNil(t, vpcprefix)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 uuid.UUID
		vpcPrefix          *VpcPrefix
		paramRelations     []string
		expectedError      bool
		expectedErrVal     error
		expectedVpcID      *uuid.UUID
		verifyChildSpanner bool
	}{
		{
			desc:               "GetById success when exists",
			id:                 vpcprefix.ID,
			vpcPrefix:          vpcprefix,
			paramRelations:     []string{},
			expectedError:      false,
			expectedVpcID:      nil,
			verifyChildSpanner: true,
		},
		{
			desc:           "GetById error when not found",
			id:             uuid.New(),
			vpcPrefix:      vpcprefix,
			paramRelations: []string{},
			expectedError:  true,
			expectedErrVal: db.ErrDoesNotExist,
			expectedVpcID:  nil,
		},
		{
			desc:           "GetById with the Vpc relation",
			id:             vpcprefix.ID,
			vpcPrefix:      vpcprefix,
			paramRelations: []string{"Vpc"},
			expectedError:  false,
			expectedVpcID:  &vpc.ID,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := vps.GetByID(ctx, nil, tc.id, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.EqualValues(t, tc.vpcPrefix.ID, got.ID)
				assert.Equal(t, tc.expectedVpcID != nil, got.Vpc != nil)
				if tc.expectedVpcID != nil {
					assert.Equal(t, *tc.expectedVpcID, got.Vpc.ID)
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

func TestVpcPrefixSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVpcPrefixSetupSchema(t, dbSession)
	ip := testVpcPrefixBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testVpcPrefixBuildSite(t, dbSession, ip, "testSite")
	tenant1 := testVpcPrefixBuildTenant(t, dbSession, "testTenant1")
	tenant2 := testVpcPrefixBuildTenant(t, dbSession, "testTenant2")
	user := testVpcPrefixBuildUser(t, dbSession, "testUser")
	ipBlock1 := testVpcPrefixBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipBlock1", &user.ID)
	ipBlock2 := testVpcPrefixBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipBlock2", &user.ID)
	vpc1 := testVpcPrefixBuildVpc(t, dbSession, ip, site, tenant1, "testVpc1")
	vpc2 := testVpcPrefixBuildVpc(t, dbSession, ip, site, tenant2, "testVpc2")

	vpsd := NewVpcPrefixDAO(dbSession)

	prefix := "192.0.2.0/24"

	totalCount := 30

	vps := []VpcPrefix{}

	for i := 0; i < totalCount; i++ {
		if i%2 == 0 {
			vpcPrefix, err := vpsd.Create(ctx, nil, VpcPrefixCreateInput{
				Name:         fmt.Sprintf("VpcPrefix-%v", i),
				TenantOrg:    "test",
				SiteID:       site.ID,
				VpcID:        vpc1.ID,
				TenantID:     tenant1.ID,
				IpBlockID:    &ipBlock1.ID,
				Prefix:       prefix,
				PrefixLength: 24,
				Status:       VpcPrefixStatusReady,
				CreatedBy:    user.ID,
			})

			assert.Nil(t, err)
			vps = append(vps, *vpcPrefix)
		} else {
			_, err := vpsd.Create(ctx, nil, VpcPrefixCreateInput{
				Name:         fmt.Sprintf("VpcPrefix-%v", i),
				TenantOrg:    "test",
				SiteID:       site.ID,
				VpcID:        vpc2.ID,
				TenantID:     tenant2.ID,
				IpBlockID:    &ipBlock2.ID,
				Prefix:       prefix,
				PrefixLength: 24,
				Status:       VpcPrefixStatusReady,
				CreatedBy:    user.ID,
			})
			assert.Nil(t, err)
		}
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		names              []string
		siteIDs            []uuid.UUID
		vpcIDs             []uuid.UUID
		tenantIDs          []uuid.UUID
		ip4BlockIDs        []uuid.UUID
		prefixes           []string
		prefixLengths      []int
		searchQuery        *string
		statuses           []string
		offset             *int
		limit              *int
		orderBy            *paginator.OrderBy
		firstEntry         *VpcPrefix
		expectedCount      int
		expectedTotal      *int
		expectedError      bool
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters returns objects",
			vpcIDs:             nil,
			expectedCount:      paginator.DefaultLimit,
			expectedTotal:      &totalCount,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:           "GetAll with relation filters returns objects",
			vpcIDs:         nil,
			expectedCount:  paginator.DefaultLimit,
			expectedTotal:  &totalCount,
			expectedError:  false,
			paramRelations: []string{VpcRelationName, SiteRelationName, TenantRelationName, IPBlockRelationName},
		},
		{
			desc:          "GetAll with Site filter returns objects",
			siteIDs:       []uuid.UUID{site.ID},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with Vpc filter returns objects",
			vpcIDs:        []uuid.UUID{vpc1.ID},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with tenant filter returns objects",
			siteIDs:       nil,
			vpcIDs:        nil,
			tenantIDs:     []uuid.UUID{tenant2.ID},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with Ipblock filter returns objects",
			ip4BlockIDs:   []uuid.UUID{ipBlock1.ID},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with limit returns objects",
			vpcIDs:        []uuid.UUID{vpc1.ID},
			offset:        cutil.GetPtr(0),
			limit:         cutil.GetPtr(5),
			expectedCount: 5,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with offset returns objects",
			vpcIDs:        []uuid.UUID{vpc1.ID},
			offset:        cutil.GetPtr(5),
			expectedCount: 10,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:   "GetAll with order by returns objects",
			vpcIDs: []uuid.UUID{vpc1.ID},
			orderBy: &paginator.OrderBy{
				Field: "name",
				Order: paginator.OrderDescending,
			},
			firstEntry:    &vps[4], // 5th entry is "VpcPrefix-8" and would appear first on descending order
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with name search query returns objects",
			searchQuery:   cutil.GetPtr("VpcPrefix-"),
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with prefix filter returns objects",
			prefixes:      []string{prefix},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with prefix length filter returns objects",
			prefixLengths: []int{24},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with status search query returns objects",
			searchQuery:   cutil.GetPtr(VpcPrefixStatusReady),
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with empty search query returns objects",
			vpcIDs:        nil,
			searchQuery:   cutil.GetPtr(""),
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with status returns objects",
			vpcIDs:        nil,
			statuses:      []string{VpcPrefixStatusReady},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := vpsd.GetAll(ctx, nil,
				VpcPrefixFilterInput{
					Names:         tc.names,
					SiteIDs:       tc.siteIDs,
					VpcIDs:        tc.vpcIDs,
					TenantIDs:     tc.tenantIDs,
					IpBlockIDs:    tc.ip4BlockIDs,
					Prefixes:      tc.prefixes,
					PrefixLengths: tc.prefixLengths,
					Statuses:      tc.statuses,
					SearchQuery:   tc.searchQuery,
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
					assert.NotNil(t, got[0].Vpc)
					assert.NotNil(t, got[0].Tenant)
					assert.NotNil(t, got[0].IPBlock)
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

func TestVpcPrefixSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVpcPrefixSetupSchema(t, dbSession)
	ip := testVpcPrefixBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testVpcPrefixBuildSite(t, dbSession, ip, "testSite")
	tenant := testVpcPrefixBuildTenant(t, dbSession, "testTenant")
	tenant2 := testVpcPrefixBuildTenant(t, dbSession, "testTenant2")
	user := testVpcPrefixBuildUser(t, dbSession, "testUser")
	ipBlock := testVpcPrefixBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipBlock", &user.ID)
	ipBlock2 := testVpcPrefixBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipBlock2", &user.ID)
	vpc := testVpcPrefixBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	vpc2 := testVpcPrefixBuildVpc(t, dbSession, ip, site, tenant2, "testVpc2")

	vpsd := NewVpcPrefixDAO(dbSession)
	prefix := "192.0.2.0/24"

	VpcPrefix1, err := vpsd.Create(ctx, nil, VpcPrefixCreateInput{
		Name:         "test1",
		TenantOrg:    "test",
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipBlock.ID,
		Prefix:       prefix,
		PrefixLength: 24,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    user.ID,
	})

	assert.Nil(t, err)
	assert.NotNil(t, VpcPrefix1)
	newPrefix := "198.0.1.0/16"
	newPrefixLength := 16

	dummyUUID := uuid.New()

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                 string
		id                   uuid.UUID
		paramName            *string
		paramOrg             *string
		paramVpcID           *uuid.UUID
		paramTenantID        *uuid.UUID
		paramIPBlockID       *uuid.UUID
		paramPrefix          *string
		paramPrefixLength    *int
		paramStatus          *string
		paramIsMissingOnSite *bool

		expectedName            *string
		expectedOrg             *string
		expectedVpcID           *uuid.UUID
		expectedTenantID        *uuid.UUID
		expectedIPBlockID       *uuid.UUID
		expectedprefix          *string
		expectedPrefixLength    *int
		expectedStatus          *string
		expectedIsMissingOnSite *bool
		expectedError           bool
		verifyChildSpanner      bool
	}{
		{
			desc:                 "can update all fields",
			id:                   VpcPrefix1.ID,
			paramName:            cutil.GetPtr("updatedName"),
			paramOrg:             cutil.GetPtr("updatedOrg"),
			paramVpcID:           &vpc2.ID,
			paramTenantID:        &tenant2.ID,
			paramIPBlockID:       &ipBlock2.ID,
			paramPrefix:          cutil.GetPtr(newPrefix),
			paramPrefixLength:    &newPrefixLength,
			paramStatus:          cutil.GetPtr(VpcPrefixStatusReady),
			paramIsMissingOnSite: cutil.GetPtr(true),

			expectedError:           false,
			expectedName:            cutil.GetPtr("updatedName"),
			expectedOrg:             cutil.GetPtr("updatedOrg"),
			expectedVpcID:           &vpc2.ID,
			expectedTenantID:        &tenant2.ID,
			expectedIPBlockID:       &ipBlock2.ID,
			expectedprefix:          cutil.GetPtr(newPrefix),
			expectedPrefixLength:    &newPrefixLength,
			expectedStatus:          cutil.GetPtr(VpcPrefixStatusReady),
			expectedIsMissingOnSite: cutil.GetPtr(true),
			verifyChildSpanner:      true,
		},
		{
			desc:                 "can update some fields",
			id:                   VpcPrefix1.ID,
			paramName:            nil,
			paramVpcID:           &vpc.ID,
			paramPrefixLength:    nil,
			paramStatus:          cutil.GetPtr(VpcPrefixStatusReady),
			paramIsMissingOnSite: nil,

			expectedError:           false,
			expectedName:            cutil.GetPtr("updatedName"),
			expectedVpcID:           &vpc.ID,
			expectedprefix:          cutil.GetPtr(newPrefix),
			expectedPrefixLength:    &newPrefixLength,
			expectedStatus:          cutil.GetPtr(VpcPrefixStatusReady),
			expectedIsMissingOnSite: cutil.GetPtr(true),
		},
		{
			desc:      "error updating when Vpc foreign key violated",
			id:        VpcPrefix1.ID,
			paramName: cutil.GetPtr("updatedName"),

			paramVpcID:           &dummyUUID,
			paramPrefix:          cutil.GetPtr(newPrefix),
			paramStatus:          cutil.GetPtr(VpcPrefixStatusReady),
			paramIsMissingOnSite: cutil.GetPtr(true),

			expectedError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := vpsd.Update(
				ctx, nil, VpcPrefixUpdateInput{
					VpcPrefixID:     tc.id,
					Name:            tc.paramName,
					TenantOrg:       tc.paramOrg,
					VpcID:           tc.paramVpcID,
					TenantID:        tc.paramTenantID,
					IpBlockID:       tc.paramIPBlockID,
					Prefix:          tc.paramPrefix,
					PrefixLength:    tc.paramPrefixLength,
					Status:          tc.paramStatus,
					IsMissingOnSite: tc.paramIsMissingOnSite,
				})
			assert.Equal(t, tc.expectedError, err != nil)
			if err == nil {
				assert.NotNil(t, got)
				assert.Equal(t, *tc.expectedName, got.Name)
				if tc.expectedOrg != nil {
					assert.Equal(t, *tc.expectedOrg, got.Org)
				}
				assert.Equal(t, *tc.expectedVpcID, got.VpcID)
				if tc.expectedTenantID != nil {
					assert.Equal(t, *tc.expectedTenantID, got.TenantID)
				}
				if tc.expectedIPBlockID != nil {
					assert.Equal(t, *tc.expectedIPBlockID, *got.IPBlockID)
				}
				if tc.expectedprefix != nil {
					assert.Equal(t, *tc.expectedprefix, got.Prefix)
				}
				assert.Equal(t, *tc.expectedPrefixLength, got.PrefixLength)
				assert.Equal(t, *tc.expectedStatus, got.Status)
				assert.Equal(t, *tc.expectedIsMissingOnSite, got.IsMissingOnSite)

				if got.Updated.String() == VpcPrefix1.Updated.String() {
					t.Errorf("got.Updated = %v, want different value", got.Updated)
				}

			} else {
				assert.Nil(t, got)
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

func testVpcPrefixSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVpcPrefixSetupSchema(t, dbSession)
	ip := testVpcPrefixBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testVpcPrefixBuildSite(t, dbSession, ip, "testSite")
	tenant := testVpcPrefixBuildTenant(t, dbSession, "testTenant")
	user := testVpcPrefixBuildUser(t, dbSession, "testUser")
	ipBlock := testVpcPrefixBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipBlock", &user.ID)
	vpc := testVpcPrefixBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	vpsd := NewVpcPrefixDAO(dbSession)

	prefix := "192.0.2.0/24"

	vpcPrefix, err := vpsd.Create(ctx, nil, VpcPrefixCreateInput{
		Name:         "test",
		TenantOrg:    "test",
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipBlock.ID,
		Prefix:       prefix,
		PrefixLength: 8,
		Status:       VpcPrefixStatusReady,
		CreatedBy:    user.ID,
	})

	assert.Nil(t, err)
	assert.NotNil(t, vpcPrefix)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		vpID               uuid.UUID
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object",
			vpID:               vpcPrefix.ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "delete non-existing object",
			vpID:          vpcPrefix.ID,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := vpsd.Delete(ctx, nil, tc.vpID)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := vpsd.GetByID(ctx, nil, tc.vpID, nil)
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

//nolint:funlen // Cases stay inline so each usage invariant is visible at the call site.
func TestVpcPrefixUsageFromInterfaces(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name                              string
		cidr                              string
		ifcCountWithoutIPs                uint64
		ips                               []string
		expectedAvailableIPs              uint64
		expectedAcquiredIPs               uint64
		expectedAvailableSmallestPrefixes uint64
		expectedAcquiredPrefixes          uint64
	}{
		{
			name:                              "pending interfaces reserve one /31 each",
			cidr:                              "10.0.0.0/28",
			ifcCountWithoutIPs:                3,
			ips:                               nil,
			expectedAvailableIPs:              16,
			expectedAcquiredIPs:               6,
			expectedAvailableSmallestPrefixes: 4,
			expectedAcquiredPrefixes:          0,
		},
		{
			name:                              "duplicate acquired prefix and pending interfaces are counted once each",
			cidr:                              "10.0.0.0/28",
			ifcCountWithoutIPs:                2,
			ips:                               []string{"10.0.0.1", "10.0.0.3", "10.0.0.1"},
			expectedAvailableIPs:              16,
			expectedAcquiredIPs:               8,
			expectedAvailableSmallestPrefixes: 3,
			expectedAcquiredPrefixes:          2,
		},
		{
			name:                              "prefix without interfaces is fully available",
			cidr:                              "10.0.0.0/28",
			ifcCountWithoutIPs:                0,
			ips:                               nil,
			expectedAvailableIPs:              16,
			expectedAcquiredIPs:               0,
			expectedAvailableSmallestPrefixes: 4,
			expectedAcquiredPrefixes:          0,
		},
		{
			name:                              "acquired IPs clamp to prefix capacity",
			cidr:                              "10.0.0.0/30",
			ifcCountWithoutIPs:                1,
			ips:                               []string{"10.0.0.1", "10.0.0.3"},
			expectedAvailableIPs:              4,
			expectedAcquiredIPs:               4,
			expectedAvailableSmallestPrefixes: 0,
			expectedAcquiredPrefixes:          2,
		},
		{
			name:                              "non-empty invalid or inapplicable addresses are not pending reservations",
			cidr:                              "10.0.0.0/28",
			ifcCountWithoutIPs:                0,
			ips:                               []string{"invalid", "10.0.0.1/31", "2001:db8::1", "192.0.2.1"},
			expectedAvailableIPs:              16,
			expectedAcquiredIPs:               0,
			expectedAvailableSmallestPrefixes: 4,
			expectedAcquiredPrefixes:          0,
		},
		{
			// A /31 is the smallest prefix length the API accepts and cannot yield a /31
			// child, so usage must still be reported rather than failing the request.
			name:                              "/31 prefix with one assigned IP does not error",
			cidr:                              "10.0.0.0/31",
			ifcCountWithoutIPs:                0,
			ips:                               []string{"10.0.0.1"},
			expectedAvailableIPs:              2,
			expectedAcquiredIPs:               2,
			expectedAvailableSmallestPrefixes: 0,
			expectedAcquiredPrefixes:          1,
		},
		{
			name:                              "/31 prefix with both addresses of the same /31 counts once",
			cidr:                              "10.0.0.0/31",
			ifcCountWithoutIPs:                0,
			ips:                               []string{"10.0.0.0", "10.0.0.1"},
			expectedAvailableIPs:              2,
			expectedAcquiredIPs:               2,
			expectedAvailableSmallestPrefixes: 0,
			expectedAcquiredPrefixes:          1,
		},
		{
			name:                              "/31 prefix with duplicate Ready and Deleting IP counts once",
			cidr:                              "10.0.0.0/31",
			ifcCountWithoutIPs:                0,
			ips:                               []string{"10.0.0.1", "10.0.0.1"},
			expectedAvailableIPs:              2,
			expectedAcquiredIPs:               2,
			expectedAvailableSmallestPrefixes: 0,
			expectedAcquiredPrefixes:          1,
		},
		{
			name:                              "/31 prefix without interfaces is fully available",
			cidr:                              "10.0.0.0/31",
			ifcCountWithoutIPs:                0,
			ips:                               nil,
			expectedAvailableIPs:              2,
			expectedAcquiredIPs:               0,
			expectedAvailableSmallestPrefixes: 0,
			expectedAcquiredPrefixes:          0,
		},
		{
			name:                              "/31 prefix with one pending interface reserves the prefix",
			cidr:                              "10.0.0.0/31",
			ifcCountWithoutIPs:                1,
			ips:                               nil,
			expectedAvailableIPs:              2,
			expectedAcquiredIPs:               2,
			expectedAvailableSmallestPrefixes: 0,
			expectedAcquiredPrefixes:          0,
		},
		{
			name:                              "/31 prefix ignores out-of-range IP",
			cidr:                              "10.0.0.0/31",
			ifcCountWithoutIPs:                0,
			ips:                               []string{"192.0.2.1"},
			expectedAvailableIPs:              2,
			expectedAcquiredIPs:               0,
			expectedAvailableSmallestPrefixes: 0,
			expectedAcquiredPrefixes:          0,
		},
	}

	for _, testCase := range tests {
		t.Run(testCase.name, func(t *testing.T) {
			t.Parallel()

			usage, err := vpcPrefixUsageFromInterfaces(context.Background(), testCase.cidr, testCase.ifcCountWithoutIPs, testCase.ips)
			require.NoError(t, err)
			require.NotNil(t, usage)
			assert.Equal(t, testCase.expectedAvailableIPs, usage.AvailableIPs)
			assert.Equal(t, testCase.expectedAcquiredIPs, usage.AcquiredIPs)
			assert.Equal(t, testCase.expectedAvailableSmallestPrefixes, usage.AvailableSmallestPrefixes)
			assert.Equal(t, testCase.expectedAcquiredPrefixes, usage.AcquiredPrefixes)
		})
	}

	t.Run("unexpected child prefix acquisition error is propagated", func(t *testing.T) {
		t.Parallel()

		usage, err := vpcPrefixUsageFromInterfaces(context.Background(), "10.0.0.1/32", 0, []string{"10.0.0.1"})
		require.Error(t, err)
		assert.Nil(t, usage)
	})
}

//nolint:funlen,paralleltest // Cases and fixtures stay inline; model tests share a PostgreSQL schema.
func TestVpcPrefixSQLDAO_GetPrefixUsage(t *testing.T) {
	dao := NewVpcPrefixDAO(nil)
	t.Run("IPv6 prefix returns no usage entry", func(t *testing.T) {
		usageByID, err := dao.GetPrefixUsage(context.Background(), nil, &VpcPrefix{
			ID:     uuid.New(),
			Prefix: "2001:db8::/64",
		})
		require.NoError(t, err)
		assert.Empty(t, usageByID)
	})

	t.Run("malformed stored prefix returns an error", func(t *testing.T) {
		prefixID := uuid.New()
		usageByID, err := dao.GetPrefixUsage(context.Background(), nil, &VpcPrefix{
			ID:     prefixID,
			Prefix: "not-a-prefix",
		})
		require.Error(t, err)
		assert.Nil(t, usageByID)
		assert.ErrorContains(t, err, prefixID.String())
		assert.ErrorContains(t, err, "not-a-prefix")
	})

	type interfaceFixture struct {
		status    string
		ipAddress *string
	}

	// prefixLength selects the VpcPrefix size under test; zero means the default /28.
	const defaultPrefixLength = 28

	tests := []struct {
		name                       string
		prefixLength               int
		interfaces                 []interfaceFixture
		expectedAvailableIPs       uint64
		expectedAcquiredIPs        uint64
		expectedAcquiredPrefixes   uint64
		expectedAvailableSmallest  uint64
		expectedFreeInterfaceSlots uint64
		expectedAdmissionAllowed   bool
	}{
		{
			name: "stale deleting rows do not exhaust prefix issue 4908",
			// Deleting rows still hold capacity; duplicate /31 addresses are de-duplicated by prefix.
			interfaces: []interfaceFixture{
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.1")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.3")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.5")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.7")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.9")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.13")},
				{status: InterfaceStatusDeleting, ipAddress: cutil.GetPtr("10.0.0.1")},
				{status: InterfaceStatusDeleting, ipAddress: cutil.GetPtr("10.0.0.3")},
				{status: InterfaceStatusDeleting, ipAddress: cutil.GetPtr("10.0.0.9")},
				{status: InterfaceStatusDeleting, ipAddress: cutil.GetPtr("10.0.0.13")},
			},
			expectedAvailableIPs:       16,
			expectedAcquiredIPs:        12,
			expectedAcquiredPrefixes:   6,
			expectedAvailableSmallest:  0,
			expectedFreeInterfaceSlots: 2,
			expectedAdmissionAllowed:   true,
		},
		{
			name: "deleting interface with a distinct IP still consumes capacity",
			interfaces: []interfaceFixture{
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.1")},
				{status: InterfaceStatusDeleting, ipAddress: cutil.GetPtr("10.0.0.3")},
			},
			expectedAvailableIPs:       16,
			expectedAcquiredIPs:        4,
			expectedAcquiredPrefixes:   2,
			expectedAvailableSmallest:  3,
			expectedFreeInterfaceSlots: 6,
			expectedAdmissionAllowed:   true,
		},
		{
			name: "pending interfaces without IPs reserve one /31 each",
			interfaces: []interfaceFixture{
				{status: InterfaceStatusPending, ipAddress: nil},
				{status: InterfaceStatusPending, ipAddress: nil},
				{status: InterfaceStatusPending, ipAddress: nil},
			},
			expectedAvailableIPs:       16,
			expectedAcquiredIPs:        6,
			expectedAcquiredPrefixes:   0,
			expectedAvailableSmallest:  4,
			expectedFreeInterfaceSlots: 5,
			expectedAdmissionAllowed:   true,
		},
		{
			name: "mixed duplicate and pending interfaces reserve unique /31s",
			interfaces: []interfaceFixture{
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.1")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.3")},
				{status: InterfaceStatusDeleting, ipAddress: cutil.GetPtr("10.0.0.1")},
				{status: InterfaceStatusPending, ipAddress: nil},
				{status: InterfaceStatusPending, ipAddress: nil},
			},
			expectedAvailableIPs:       16,
			expectedAcquiredIPs:        8,
			expectedAcquiredPrefixes:   2,
			expectedAvailableSmallest:  3,
			expectedFreeInterfaceSlots: 4,
			expectedAdmissionAllowed:   true,
		},
		{
			name: "usage clamps when acquired and pending interfaces exceed capacity",
			interfaces: []interfaceFixture{
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.1")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.3")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.5")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.7")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.9")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.11")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.13")},
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.15")},
				{status: InterfaceStatusPending, ipAddress: nil},
			},
			expectedAvailableIPs:       16,
			expectedAcquiredIPs:        16,
			expectedAcquiredPrefixes:   8,
			expectedAvailableSmallest:  0,
			expectedFreeInterfaceSlots: 0,
			expectedAdmissionAllowed:   false,
		},
		{
			// A /31 is the smallest VpcPrefix the API accepts and holds exactly one
			// Interface, so a single Ready row consumes it without erroring.
			name:         "/31 prefix with one Ready interface is fully acquired issue 4908 follow-up",
			prefixLength: 31,
			interfaces: []interfaceFixture{
				{status: InterfaceStatusReady, ipAddress: cutil.GetPtr("10.0.0.1")},
			},
			expectedAvailableIPs:       2,
			expectedAcquiredIPs:        2,
			expectedAcquiredPrefixes:   1,
			expectedAvailableSmallest:  0,
			expectedFreeInterfaceSlots: 0,
			expectedAdmissionAllowed:   false,
		},
		{
			name:                       "/31 prefix without interfaces admits one interface",
			prefixLength:               31,
			interfaces:                 nil,
			expectedAvailableIPs:       2,
			expectedAcquiredIPs:        0,
			expectedAcquiredPrefixes:   0,
			expectedAvailableSmallest:  0,
			expectedFreeInterfaceSlots: 1,
			expectedAdmissionAllowed:   true,
		},
	}

	for _, testCase := range tests {
		t.Run(testCase.name, func(t *testing.T) {
			dbSession := testVpcPrefixInitDB(t)
			t.Cleanup(func() {
				dbSession.Close()
			})
			testInterfaceSetupSchema(t, dbSession)

			infrastructureProvider := testInstanceBuildInfrastructureProvider(t, dbSession, "issue-4908-provider")
			site := testInstanceBuildSite(t, dbSession, infrastructureProvider, "issue-4908-site")
			tenant := testInstanceBuildTenant(t, dbSession, "issue-4908-tenant")
			vpc := testInstanceBuildVpc(t, dbSession, infrastructureProvider, site, tenant, "issue-4908-vpc")
			user := testInstanceBuildUser(t, dbSession, "issue-4908-user")
			instanceType := testInstanceBuildInstanceType(t, dbSession, infrastructureProvider, "issue-4908-instance-type")
			machine := testMachineBuildMachine(t, dbSession, infrastructureProvider.ID, site.ID, &instanceType.ID, cutil.GetPtr("issue-4908-machine-type"))
			operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "issue-4908-os")
			instance := TestBuildInstance(t, dbSession, "issue-4908-instance", tenant, infrastructureProvider, site, instanceType, vpc, machine, operatingSystem)
			instance.Status = InstanceStatusConfiguring
			_, err := dbSession.DB.NewUpdate().Model(instance).Column("status").Where("id = ?", instance.ID).Exec(context.Background())
			require.NoError(t, err)

			prefixLength := testCase.prefixLength
			if prefixLength == 0 {
				prefixLength = defaultPrefixLength
			}

			vpcPrefix, err := NewVpcPrefixDAO(dbSession).Create(context.Background(), nil, VpcPrefixCreateInput{
				VpcPrefixID:  nil,
				Name:         "issue-4908-prefix",
				TenantOrg:    tenant.Org,
				SiteID:       site.ID,
				VpcID:        vpc.ID,
				TenantID:     tenant.ID,
				IpBlockID:    nil,
				Prefix:       fmt.Sprintf("10.0.0.0/%d", prefixLength),
				PrefixLength: prefixLength,
				Status:       VpcPrefixStatusReady,
				CreatedBy:    user.ID,
			})
			require.NoError(t, err)

			for _, interfaceFixture := range testCase.interfaces {
				ifc := TestBuildInterface(t, dbSession, instance, nil, &vpcPrefix.ID, true, interfaceFixture.status)
				if interfaceFixture.ipAddress != nil {
					ifc.IPAddresses = []string{*interfaceFixture.ipAddress}
					_, err = dbSession.DB.NewUpdate().Model(ifc).Column("ip_addresses").Where("id = ?", ifc.ID).Exec(context.Background())
					require.NoError(t, err)
				}
			}

			usageByID, err := NewVpcPrefixDAO(dbSession).GetPrefixUsage(context.Background(), nil, vpcPrefix)
			require.NoError(t, err)

			usage := usageByID[vpcPrefix.ID]
			require.NotNil(t, usage)
			assert.Equal(t, testCase.expectedAvailableIPs, usage.AvailableIPs)
			assert.Equal(t, testCase.expectedAcquiredIPs, usage.AcquiredIPs)
			assert.Equal(t, testCase.expectedAcquiredPrefixes, usage.AcquiredPrefixes)
			assert.Equal(t, testCase.expectedAvailableSmallest, usage.AvailableSmallestPrefixes)
			assert.Equal(t, testCase.expectedFreeInterfaceSlots, (usage.AvailableIPs-usage.AcquiredIPs)/vpcPrefixIPsPerInterface)

			admissionAllowed := usage.AcquiredIPs+vpcPrefixIPsPerInterface <= usage.AvailableIPs
			assert.Equal(t, testCase.expectedAdmissionAllowed, admissionAllowed)
		})
	}
}

func TestVpcPrefixSQLDAO_ClearDeleted(t *testing.T) {
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVpcPrefixSetupSchema(t, dbSession)

	ip := testVpcPrefixBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testVpcPrefixBuildSite(t, dbSession, ip, "testSite")
	tenant := testVpcPrefixBuildTenant(t, dbSession, "testTenant")
	user := testVpcPrefixBuildUser(t, dbSession, "testUser")
	ipBlock := testVpcPrefixBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipBlock", &user.ID)
	vpc := testVpcPrefixBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	vpcPrefixDAO := NewVpcPrefixDAO(dbSession)

	vpcPrefix, err := vpcPrefixDAO.Create(ctx, nil, VpcPrefixCreateInput{
		Name:         "test-clear-deleted",
		TenantOrg:    "test",
		SiteID:       site.ID,
		VpcID:        vpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipBlock.ID,
		Prefix:       "192.0.2.0/24",
		PrefixLength: 24,
		Status:       VpcPrefixStatusError,
		CreatedBy:    user.ID,
	})
	require.NoError(t, err)
	require.NoError(t, vpcPrefixDAO.Delete(ctx, nil, vpcPrefix.ID))

	t.Run("clears soft-delete marker", func(t *testing.T) {
		cleared, clearErr := vpcPrefixDAO.Clear(ctx, nil, VpcPrefixClearInput{
			VpcPrefixID: vpcPrefix.ID,
			Deleted:     true,
		})
		require.NoError(t, clearErr)
		require.NotNil(t, cleared)
		assert.Nil(t, cleared.Deleted)

		updated, updateErr := vpcPrefixDAO.Update(ctx, nil, VpcPrefixUpdateInput{
			VpcPrefixID:     vpcPrefix.ID,
			Status:          cutil.GetPtr(VpcPrefixStatusReady),
			IsMissingOnSite: cutil.GetPtr(false),
		})
		require.NoError(t, updateErr)
		assert.Equal(t, VpcPrefixStatusReady, updated.Status)
		assert.False(t, updated.IsMissingOnSite)
	})

	t.Run("returns not found for unknown VPC Prefix", func(t *testing.T) {
		_, clearErr := vpcPrefixDAO.Clear(ctx, nil, VpcPrefixClearInput{
			VpcPrefixID: uuid.New(),
			Deleted:     true,
		})
		assert.ErrorIs(t, clearErr, db.ErrDoesNotExist)
	})
}
