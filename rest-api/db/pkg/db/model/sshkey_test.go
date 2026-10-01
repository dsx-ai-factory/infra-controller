// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
)

// reset the tables needed for SSHKey tests
func testSSHKeySetupSchema(t *testing.T, dbSession *db.Session) {
	testInstanceSetupSchema(t, dbSession)
	// create the SSHKey table
	err := dbSession.DB.ResetModel(context.Background(), (*SSHKey)(nil))
	assert.Nil(t, err)
}

func TestSSHKeySQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeySetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	sksd := NewSSHKeyDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		sks                []SSHKey
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			sks: []SSHKey{
				{
					Name: "test", Org: "test", TenantID: tenant.ID, PublicKey: "test", Fingerprint: cutil.GetPtr("test"), CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple, some with null fields",
			sks: []SSHKey{
				{
					Name: "test", Org: "test", TenantID: tenant.ID, PublicKey: "test", CreatedBy: user.ID,
				},
				{
					Name: "test", Org: "test", TenantID: tenant.ID, PublicKey: "test", Fingerprint: cutil.GetPtr("test"), Expires: cutil.GetPtr(time.Now()), CreatedBy: user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "failure - foreign key violation on tenant_id",
			sks: []SSHKey{
				{
					Name: "test", Org: "test", TenantID: uuid.New(), PublicKey: "test", Fingerprint: cutil.GetPtr("test"), CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, sk := range tc.sks {
				sk, err := sksd.Create(
					ctx,
					nil,
					SSHKeyCreateInput{
						Name:        sk.Name,
						TenantOrg:   sk.Org,
						TenantID:    sk.TenantID,
						PublicKey:   sk.PublicKey,
						Fingerprint: sk.Fingerprint,
						Expires:     sk.Expires,
						CreatedBy:   sk.CreatedBy,
					},
				)
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, sk)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestSSHKeySQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeySetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	sksd := NewSSHKeyDAO(dbSession)
	sk1, err := sksd.Create(
		ctx,
		nil,
		SSHKeyCreateInput{
			Name:        "test",
			TenantOrg:   "test",
			TenantID:    tenant.ID,
			PublicKey:   "testkey",
			Fingerprint: nil,
			Expires:     cutil.GetPtr(time.Now()),
			CreatedBy:   user.ID,
		},
	)
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		sgID               uuid.UUID
		includeRelations   []string
		expectNotNilTenant bool
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:               "success without relations",
			sgID:               sk1.ID,
			includeRelations:   []string{},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:               "success with relations",
			sgID:               sk1.ID,
			includeRelations:   []string{"Tenant"},
			expectError:        false,
			expectNotNilTenant: true,
		},
		{
			desc:             "error when not found",
			sgID:             uuid.New(),
			includeRelations: []string{},
			expectError:      true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := sksd.GetByID(ctx, nil, tc.sgID, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				if tc.expectNotNilTenant {
					assert.NotNil(t, got.Tenant)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeySQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	TestSetupSchema(t, dbSession)

	ipOrg1 := "test-ip-org-1"
	tnOrg := "test-tenant-org-1"

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", ipOrg1, ipu.ID)
	assert.NotNil(t, ip)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe1@test.com"), cutil.GetPtr("John1"), cutil.GetPtr("Doe2"))
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)
	assert.NotNil(t, tn)

	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	site1 := testBuildSite(t, dbSession, nil, ip.ID, "test-site-1", "Test Site-1", ip.Org, ipu.ID)

	// Build SSHKeyGroup
	skg1 := testBuildSSHKeyGroup(t, dbSession, "test1", cutil.GetPtr("testdesc"), tnOrg, tn.ID, cutil.GetPtr("test-version"), SSHKeyGroupStatusSyncing, tnu.ID)

	// Build SSHKeyGroup
	skg2 := testBuildSSHKeyGroup(t, dbSession, "test2", cutil.GetPtr("testdesc"), tnOrg, tn.ID, cutil.GetPtr("test-version"), SSHKeyGroupStatusSyncing, tnu.ID)

	// Build SSHKeyGroupSiteAssociation
	skgsa1 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, site1.ID, cutil.GetPtr("test-version"), SSHKeyGroupSiteAssociationStatusSynced, tnu.ID)
	assert.NotNil(t, skgsa1)

	// Build SSHKeyGroupSiteAssociation
	skgsa2 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg2.ID, site1.ID, cutil.GetPtr("test-version"), SSHKeyGroupSiteAssociationStatusSynced, tnu.ID)
	assert.NotNil(t, skgsa2)

	skg1Sshkeys := []SSHKey{}
	skg2Sshkeys := []SSHKey{}

	totalSshkeys := 25

	sksd := NewSSHKeyDAO(dbSession)
	for i := 1; i <= totalSshkeys; i++ {
		sk1, err := sksd.Create(
			ctx,
			nil,
			SSHKeyCreateInput{
				Name:        fmt.Sprintf("test-%d", i),
				TenantOrg:   "testorg",
				TenantID:    tenant.ID,
				PublicKey:   fmt.Sprintf("testkey-%d", i),
				Fingerprint: cutil.GetPtr(fmt.Sprintf("fingerprint-%d", i)),
				Expires:     nil,
				CreatedBy:   user.ID,
			},
		)
		assert.Nil(t, err)
		assert.NotNil(t, sk1)
		if i < 5 {
			// Build SSHKeyAssociation
			ska1 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg1.ID, tn.ID)
			assert.NotNil(t, ska1)
			skg1Sshkeys = append(skg1Sshkeys, *sk1)
		} else {
			// Build SSHKeyAssociation
			ska2 := testBuildSSHKeyAssociation(t, dbSession, sk1.ID, skg2.ID, tn.ID)
			assert.NotNil(t, ska2)
			skg2Sshkeys = append(skg2Sshkeys, *sk1)
		}
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc             string
		includeRelations []string

		paramNames          []string
		paramFingerprints   []string
		paramOrgs           []string
		paramTenantIDs      []uuid.UUID
		paramSSHKeyGroupIDs []uuid.UUID
		searchQuery         *string

		paramOffset  *int
		paramLimit   *int
		paramOrderBy *paginator.OrderBy

		expectCnt             int
		expectTotal           int
		expectFirstObjectName string

		expectNotNilTenant bool
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:                  "getall with fingerprint filter but no relations returns objects",
			paramFingerprints:     []string{"fingerprint-11"},
			paramTenantIDs:        []uuid.UUID{tenant.ID},
			includeRelations:      []string{},
			expectFirstObjectName: "test-11",
			expectError:           false,
			expectTotal:           1,
			expectCnt:             1,
			verifyChildSpanner:    true,
		},
		{
			desc:                  "getall with name filter but no relations returns objects",
			paramNames:            []string{"test-1"},
			paramTenantIDs:        []uuid.UUID{tenant.ID},
			includeRelations:      []string{},
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           1,
			expectCnt:             1,
		},
		{
			desc:                  "getall with filters but no relations returns objects",
			paramTenantIDs:        []uuid.UUID{tenant.ID},
			includeRelations:      []string{},
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
		{
			desc: "getall with tenant, and org returns objects",

			paramTenantIDs: []uuid.UUID{tenant.ID},
			paramOrgs:      []string{"testorg"},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			includeRelations:      []string{},
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
		{
			desc:             "getall with filters and relations returns objects",
			includeRelations: []string{"Tenant"},

			paramTenantIDs: []uuid.UUID{tenant.ID},

			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
			expectNotNilTenant:    true,
		},
		{
			desc:                  "getall with sshkeygroupid filter returns objects",
			includeRelations:      []string{"Tenant"},
			paramSSHKeyGroupIDs:   []uuid.UUID{skg1.ID},
			paramTenantIDs:        []uuid.UUID{tenant.ID},
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           len(skg1Sshkeys),
			expectCnt:             len(skg1Sshkeys),
			expectNotNilTenant:    true,
		},
		{
			desc:             "getall with offset, limit returns objects",
			includeRelations: []string{},

			paramOffset: cutil.GetPtr(10),
			paramLimit:  cutil.GetPtr(10),
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			paramTenantIDs:        []uuid.UUID{tenant.ID},
			expectFirstObjectName: "test-11",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             10,
		},
		{
			desc:             "case when no objects are returned",
			includeRelations: []string{},
			expectError:      false,
			paramTenantIDs:   []uuid.UUID{uuid.New()},
			expectTotal:      0,
			expectCnt:        0,
		},
		{
			desc:                  "getall with name search query returns objects",
			paramTenantIDs:        nil,
			includeRelations:      []string{},
			searchQuery:           cutil.GetPtr("test-"),
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
		{
			desc:                  "getall with empty search query returns objects",
			paramTenantIDs:        nil,
			includeRelations:      []string{},
			searchQuery:           cutil.GetPtr(""),
			expectFirstObjectName: "test-1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			objs, tot, err := sksd.GetAll(
				ctx,
				nil,
				SSHKeyFilterInput{
					Names:          tc.paramNames,
					TenantOrgs:     tc.paramOrgs,
					TenantIDs:      tc.paramTenantIDs,
					SSHKeyGroupIDs: tc.paramSSHKeyGroupIDs,
					Fingerprints:   tc.paramFingerprints,
					SearchQuery:    tc.searchQuery,
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
				assert.Equal(t, tc.expectFirstObjectName, objs[0].Name)
				if tc.expectNotNilTenant {
					assert.NotNil(t, objs[0].Tenant)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}

}

func TestSSHKeySQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeySetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	tenant1 := testOperatingSystemBuildTenant(t, dbSession, "testTenant1")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	sksd := NewSSHKeyDAO(dbSession)
	sk1, err := sksd.Create(
		ctx,
		nil,
		SSHKeyCreateInput{
			Name:      "test",
			TenantOrg: "test",
			TenantID:  tenant.ID,
			PublicKey: "testkey",
			CreatedBy: user.ID,
		},
	)
	assert.Nil(t, err)

	now := time.Now().UTC()

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc string
		id   uuid.UUID

		paramName        *string
		paramOrg         *string
		paramTenantID    *uuid.UUID
		paramPublicKey   *string
		paramFingerprint *string
		paramExpires     *time.Time

		expectedName        *string
		expectedOrg         *string
		expectedTenantID    *uuid.UUID
		expectedIsGlobal    *bool
		expectedPublicKey   *string
		expectedFingerprint *string
		expectedExpires     *time.Time

		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:             "can update all fields",
			id:               sk1.ID,
			paramName:        cutil.GetPtr("updatedName"),
			paramOrg:         cutil.GetPtr("updatedOrg"),
			paramTenantID:    &tenant1.ID,
			paramPublicKey:   cutil.GetPtr("updatedPublicKey"),
			paramFingerprint: cutil.GetPtr("updatedFingerprint"),
			paramExpires:     cutil.GetPtr(now),

			expectedName:        cutil.GetPtr("updatedName"),
			expectedOrg:         cutil.GetPtr("updatedOrg"),
			expectedTenantID:    &tenant1.ID,
			expectedIsGlobal:    cutil.GetPtr(false),
			expectedPublicKey:   cutil.GetPtr("updatedPublicKey"),
			expectedFingerprint: cutil.GetPtr("updatedFingerprint"),
			expectedExpires:     cutil.GetPtr(now),

			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:        "error when ID not found",
			id:          uuid.New(),
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := sksd.Update(
				ctx,
				nil,
				SSHKeyUpdateInput{
					SSHKeyID:    tc.id,
					Name:        tc.paramName,
					TenantOrg:   tc.paramOrg,
					TenantID:    tc.paramTenantID,
					PublicKey:   tc.paramPublicKey,
					Fingerprint: tc.paramFingerprint,
					Expires:     tc.paramExpires,
				},
			)
			assert.Equal(t, tc.expectError, err != nil)
			if err == nil {
				assert.Equal(t, *tc.expectedName, got.Name)
				assert.Equal(t, *tc.expectedOrg, got.Org)
				assert.Equal(t, tc.expectedTenantID.String(), got.TenantID.String())

				assert.Equal(t, *tc.expectedPublicKey, got.PublicKey)

				assert.Equal(t, *tc.expectedFingerprint, *got.Fingerprint)
				assert.Equal(t, tc.expectedExpires.Unix(), got.Expires.UTC().Unix())
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeySQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeySetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	sksd := NewSSHKeyDAO(dbSession)
	sk1, err := sksd.Create(
		ctx,
		nil,
		SSHKeyCreateInput{
			Name:      "test",
			TenantOrg: "test",
			TenantID:  tenant.ID,
			PublicKey: "testkey",
			CreatedBy: user.ID,
		},
	)
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 uuid.UUID
		expectedError      bool
		checkPublicKey     bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object",
			id:                 sk1.ID,
			expectedError:      false,
			checkPublicKey:     true,
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
			err := sksd.Delete(ctx, nil, tc.id)
			require.Equal(t, tc.expectedError, err != nil, err)
			if !tc.expectedError {
				tmp, err := sksd.GetByID(ctx, nil, tc.id, nil)
				assert.NotNil(t, err)
				assert.Nil(t, tmp)
			}
			if tc.checkPublicKey {
				sk := &SSHKey{}
				err := db.GetIDB(nil, dbSession).NewSelect().Model(sk).WhereAllWithDeleted().Where("sk.id = ?", tc.id).Scan(ctx)
				assert.Nil(t, err)
				assert.Equal(t, "", sk.PublicKey)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
