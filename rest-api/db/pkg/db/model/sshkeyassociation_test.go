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

// reset the tables needed for SSHKeyAssociationAssociation tests
func testSSHKeyAssociationSetupSchema(t *testing.T, dbSession *db.Session) {
	testSSHKeySetupSchema(t, dbSession)
	testSSHKeyGroupSetupSchema(t, dbSession)
	testSSHKeyGroupSiteAssociationSetupSchema(t, dbSession)
	// create the SSHKeyAssociation table
	err := dbSession.DB.ResetModel(context.Background(), (*SSHKeyAssociation)(nil))
	assert.Nil(t, err)
}

func TestSSHKeyAssociationSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyAssociationSetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	sshKey := testBuildSSHKey(t, dbSession, "test", "test", tenant.ID, "test", cutil.GetPtr("test"), nil, user.ID)
	sshKey2 := testBuildSSHKey(t, dbSession, "test2", "test2", tenant.ID, "test2", cutil.GetPtr("test2"), nil, user.ID)

	sshKeyGroup := testBuildSSHKeyGroup(t, dbSession, "test", cutil.GetPtr("test"), "test", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)
	sshKeyGroup2 := testBuildSSHKeyGroup(t, dbSession, "test2", cutil.GetPtr("test2"), "test2", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)

	sksd := NewSSHKeyAssociationDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		sks                []SSHKeyAssociation
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			sks: []SSHKeyAssociation{
				{
					SSHKeyID: sshKey.ID, SSHKeyGroupID: sshKeyGroup.ID, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple",
			sks: []SSHKeyAssociation{
				{
					SSHKeyID: sshKey.ID, SSHKeyGroupID: sshKeyGroup.ID, CreatedBy: user.ID,
				},
				{
					SSHKeyID: sshKey2.ID, SSHKeyGroupID: sshKeyGroup2.ID, CreatedBy: user.ID,
				},
			},
			expectError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, ska := range tc.sks {
				ska, err := sksd.Create(ctx, nil, SSHKeyAssociationCreateInput{
					SSHKeyID:      ska.SSHKeyID,
					SSHKeyGroupID: ska.SSHKeyGroupID,
					CreatedBy:     ska.CreatedBy,
				})
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, ska)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestSSHKeyAssociationSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyAssociationSetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	skasd := NewSSHKeyAssociationDAO(dbSession)

	sshKey := testBuildSSHKey(t, dbSession, "test", "test", tenant.ID, "test", cutil.GetPtr("test"), nil, user.ID)
	sshKeyGroup := testBuildSSHKeyGroup(t, dbSession, "test", cutil.GetPtr("test"), "test", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)

	ska1, err := skasd.Create(ctx, nil, SSHKeyAssociationCreateInput{
		SSHKeyID:      sshKey.ID,
		SSHKeyGroupID: sshKeyGroup.ID,
		CreatedBy:     user.ID,
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                    string
		skaID                   uuid.UUID
		includeRelations        []string
		expectNotNilSSHKey      bool
		expectNotNilSSHKeyGroup bool
		expectError             bool
		verifyChildSpanner      bool
	}{
		{
			desc:               "success without relations",
			skaID:              ska1.ID,
			includeRelations:   []string{},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:                    "success with relations",
			skaID:                   ska1.ID,
			includeRelations:        []string{SSHKeyRelationName, SSHKeyGroupRelationName},
			expectError:             false,
			expectNotNilSSHKey:      true,
			expectNotNilSSHKeyGroup: true,
		},
		{
			desc:             "error when not found",
			skaID:            uuid.New(),
			includeRelations: []string{},
			expectError:      true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := skasd.GetByID(ctx, nil, tc.skaID, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				if tc.expectNotNilSSHKey {
					assert.NotNil(t, got.SSHKey)
				}
				if tc.expectNotNilSSHKeyGroup {
					assert.NotNil(t, got.SSHKeyGroup)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyAssociationSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyAssociationSetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	sks := []*SSHKey{}
	skgs := []*SSHKeyGroup{}
	sksd := NewSSHKeyDAO(dbSession)
	skgsd := NewSSHKeyGroupDAO(dbSession)
	skasd := NewSSHKeyAssociationDAO(dbSession)
	for i := 1; i <= 25; i++ {
		sk1, err := sksd.Create(
			ctx,
			nil,
			SSHKeyCreateInput{
				Name:      fmt.Sprintf("test-%d", i),
				TenantOrg: "testorg",
				TenantID:  tenant.ID,
				PublicKey: fmt.Sprintf("testkey-%d", i),
				CreatedBy: user.ID,
			},
		)
		sks = append(sks, sk1)
		assert.Nil(t, err)
		assert.NotNil(t, sk1)

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

		ska1, err := skasd.Create(ctx, nil, SSHKeyAssociationCreateInput{
			SSHKeyID:      sk1.ID,
			SSHKeyGroupID: skg1.ID,
			CreatedBy:     user.ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, ska1)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc             string
		includeRelations []string

		paramFilter SSHKeyAssociationFilterInput

		paramPage paginator.PageInput

		expectCnt                      int
		expectTotal                    int
		expectFirstObjectSSHKeyID      string
		expectFirstObjectSSHKeyGroupID string

		expectNotNilSSHKey      bool
		expectNotNilSSHKeyGroup bool
		expectError             bool
		verifyChildSpanner      bool
	}{
		{
			desc: "getall with sshkeyid filters but no relations returns objects",
			paramFilter: SSHKeyAssociationFilterInput{
				SSHKeyIDs: []uuid.UUID{sks[0].ID, sks[1].ID},
			},
			includeRelations:          []string{},
			expectFirstObjectSSHKeyID: sks[0].ID.String(),
			expectError:               false,
			expectTotal:               2,
			expectCnt:                 2,
			verifyChildSpanner:        true,
		},
		{
			desc:             "getall with sshkeyid filters and relations returns objects",
			includeRelations: []string{SSHKeyRelationName, SSHKeyGroupRelationName},
			paramPage: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "updated",
					Order: paginator.OrderAscending,
				},
			},
			paramFilter: SSHKeyAssociationFilterInput{
				SSHKeyIDs: []uuid.UUID{sks[0].ID},
			},
			expectFirstObjectSSHKeyID: sks[0].ID.String(),
			expectError:               false,
			expectTotal:               1,
			expectCnt:                 1,
			expectNotNilSSHKey:        true,
			expectNotNilSSHKeyGroup:   true,
		},
		{
			desc:             "getall with sshkeygroupid filters and relations returns objects",
			includeRelations: []string{SSHKeyRelationName, SSHKeyGroupRelationName},
			paramPage: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "updated",
					Order: paginator.OrderAscending,
				},
			},
			paramFilter: SSHKeyAssociationFilterInput{
				SSHKeyGroupIDs: []uuid.UUID{skgs[0].ID, skgs[1].ID},
			},
			expectFirstObjectSSHKeyGroupID: skgs[0].ID.String(),
			expectError:                    false,
			expectTotal:                    2,
			expectCnt:                      2,
			expectNotNilSSHKey:             true,
			expectNotNilSSHKeyGroup:        true,
		},
		{
			desc:             "getall with offset, limit returns objects",
			includeRelations: []string{},

			paramPage: paginator.PageInput{
				Offset: cutil.GetPtr(10),
				Limit:  cutil.GetPtr(10),
				OrderBy: &paginator.OrderBy{
					Field: "updated",
					Order: paginator.OrderAscending,
				},
			},
			paramFilter:               SSHKeyAssociationFilterInput{},
			expectFirstObjectSSHKeyID: sks[10].ID.String(),
			expectError:               false,
			expectTotal:               25,
			expectCnt:                 10,
		},
		{
			desc:             "case when no objects are returned",
			includeRelations: []string{},
			expectError:      false,
			paramFilter: SSHKeyAssociationFilterInput{
				SSHKeyIDs: []uuid.UUID{uuid.New()},
			},
			paramPage:   paginator.PageInput{},
			expectTotal: 0,
			expectCnt:   0,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			objs, tot, err := skasd.GetAll(ctx, nil, tc.paramFilter, tc.paramPage, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			assert.Equal(t, tc.expectCnt, len(objs))
			assert.Equal(t, tc.expectTotal, tot)
			if len(objs) > 0 {
				if tc.expectFirstObjectSSHKeyID != "" {
					assert.Equal(t, tc.expectFirstObjectSSHKeyID, objs[0].SSHKeyID.String())
				}
				if tc.expectFirstObjectSSHKeyGroupID != "" {
					assert.Equal(t, tc.expectFirstObjectSSHKeyGroupID, objs[0].SSHKeyGroupID.String())
				}
				if tc.expectNotNilSSHKey {
					assert.NotNil(t, objs[0].SSHKey)
				}
				if tc.expectNotNilSSHKeyGroup {
					assert.NotNil(t, objs[0].SSHKeyGroup)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyAssociationSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyAssociationSetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	skasd := NewSSHKeyAssociationDAO(dbSession)

	sshKey := testBuildSSHKey(t, dbSession, "test", "test", tenant.ID, "test", cutil.GetPtr("test"), nil, user.ID)
	sshKey2 := testBuildSSHKey(t, dbSession, "test2", "test2", tenant.ID, "test2", cutil.GetPtr("test2"), nil, user.ID)

	sshKeyGroup := testBuildSSHKeyGroup(t, dbSession, "test", cutil.GetPtr("test"), "test", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)
	sshKeyGroup2 := testBuildSSHKeyGroup(t, dbSession, "test2", cutil.GetPtr("test2"), "test2", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)

	ska1, err := skasd.Create(ctx, nil, SSHKeyAssociationCreateInput{
		SSHKeyID:      sshKey.ID,
		SSHKeyGroupID: sshKeyGroup.ID,
		CreatedBy:     user.ID,
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc string
		id   uuid.UUID

		paramSSHKeyID      *uuid.UUID
		paramSSHKeyGroupID *uuid.UUID

		expectedSSHKeyID      *uuid.UUID
		expectedSSHKeyGroupID *uuid.UUID

		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:                  "can update all fields",
			id:                    ska1.ID,
			paramSSHKeyID:         cutil.GetPtr(sshKey2.ID),
			paramSSHKeyGroupID:    cutil.GetPtr(sshKeyGroup2.ID),
			expectedSSHKeyID:      cutil.GetPtr(sshKey2.ID),
			expectedSSHKeyGroupID: cutil.GetPtr(sshKeyGroup2.ID),
			expectError:           false,
			verifyChildSpanner:    true,
		},
		{
			desc:        "error when ID not found",
			id:          uuid.New(),
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := skasd.Update(ctx, nil, SSHKeyAssociationUpdateInput{
				SSHKeyAssociationID: tc.id,
				SSHKeyID:            tc.paramSSHKeyID,
				SSHKeyGroupID:       tc.paramSSHKeyGroupID,
			})
			assert.Equal(t, tc.expectError, err != nil)
			if err == nil {
				assert.Equal(t, tc.expectedSSHKeyID.String(), got.SSHKeyID.String())
				assert.Equal(t, tc.expectedSSHKeyGroupID.String(), got.SSHKeyGroupID.String())
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSSHKeyAssociationSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testSSHKeyAssociationSetupSchema(t, dbSession)
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")

	skasd := NewSSHKeyAssociationDAO(dbSession)
	sshKey := testBuildSSHKey(t, dbSession, "test", "test", tenant.ID, "test", cutil.GetPtr("test"), nil, user.ID)
	sshKeyGroup := testBuildSSHKeyGroup(t, dbSession, "test", cutil.GetPtr("test"), "test", tenant.ID, nil, SSHKeyGroupStatusSyncing, user.ID)

	ska1, err := skasd.Create(ctx, nil, SSHKeyAssociationCreateInput{
		SSHKeyID:      sshKey.ID,
		SSHKeyGroupID: sshKeyGroup.ID,
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
			id:                 ska1.ID,
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
			err := skasd.Delete(ctx, nil, tc.id)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := skasd.GetByID(ctx, nil, tc.id, nil)
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
