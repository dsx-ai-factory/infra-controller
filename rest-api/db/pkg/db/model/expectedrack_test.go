// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"testing"

	"github.com/stretchr/testify/assert"
	otrace "go.opentelemetry.io/otel/trace"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
)

func TestExpectedRack_FromProto(t *testing.T) {
	preservedID := uuid.New()
	preservedSiteID := uuid.New()

	t.Run("nil proto leaves receiver unchanged", func(t *testing.T) {
		er := &ExpectedRack{ID: preservedID, SiteID: preservedSiteID, RackID: "preserved-rack", Name: "preserved-name"}
		er.FromProto(nil)

		assert.Equal(t, preservedID, er.ID)
		assert.Equal(t, preservedSiteID, er.SiteID)
		assert.Equal(t, "preserved-rack", er.RackID)
		assert.Equal(t, "preserved-name", er.Name)
	})

	t.Run("nil RackId leaves er.RackID unchanged", func(t *testing.T) {
		er := &ExpectedRack{RackID: "preserved"}
		er.FromProto(&corev1.ExpectedRack{
			RackId:        nil,
			RackProfileId: &corev1.RackProfileId{Id: "type-A"},
		})

		assert.Equal(t, "preserved", er.RackID)
		assert.Equal(t, "type-A", er.RackProfileID)
	})

	t.Run("empty RackId leaves er.RackID unchanged", func(t *testing.T) {
		er := &ExpectedRack{RackID: "preserved"}
		er.FromProto(&corev1.ExpectedRack{
			RackId:        &corev1.RackId{Id: ""},
			RackProfileId: &corev1.RackProfileId{Id: "type-A"},
		})

		assert.Equal(t, "preserved", er.RackID)
		assert.Equal(t, "type-A", er.RackProfileID)
	})

	t.Run("populates all proto fields", func(t *testing.T) {
		er := &ExpectedRack{}
		er.FromProto(&corev1.ExpectedRack{
			RackId:        &corev1.RackId{Id: "rack-1"},
			RackProfileId: &corev1.RackProfileId{Id: "type-A"},
			Metadata: &corev1.Metadata{
				Name:        "rack-name",
				Description: "primary rack",
				Labels: []*corev1.Label{
					{Key: "env", Value: cutil.GetPtr("prod")},
				},
			},
		})

		assert.Equal(t, "rack-1", er.RackID)
		assert.Equal(t, "type-A", er.RackProfileID)
		assert.Equal(t, "rack-name", er.Name)
		assert.Equal(t, "primary rack", er.Description)
		assert.Equal(t, Labels{"env": "prod"}, er.Labels)
	})

	t.Run("nil Metadata clears Name/Description and Labels", func(t *testing.T) {
		er := &ExpectedRack{Name: "stale-name", Description: "stale-desc", Labels: map[string]string{"old": "val"}}
		er.FromProto(&corev1.ExpectedRack{
			RackId:        &corev1.RackId{Id: "rack-1"},
			RackProfileId: &corev1.RackProfileId{Id: "type-A"},
			Metadata:      nil,
		})

		assert.Equal(t, "rack-1", er.RackID)
		assert.Equal(t, "", er.Name)
		assert.Equal(t, "", er.Description)
		assert.Nil(t, er.Labels)
	})

	t.Run("does not touch SiteID, ID, or CreatedBy", func(t *testing.T) {
		creator := uuid.New()
		er := &ExpectedRack{
			ID:        preservedID,
			SiteID:    preservedSiteID,
			CreatedBy: creator,
		}
		er.FromProto(&corev1.ExpectedRack{
			RackId:        &corev1.RackId{Id: "rack-1"},
			RackProfileId: &corev1.RackProfileId{Id: "type-A"},
		})

		assert.Equal(t, preservedID, er.ID)
		assert.Equal(t, preservedSiteID, er.SiteID)
		assert.Equal(t, creator, er.CreatedBy)
	})
}

// reset the tables needed for ExpectedRack tests
func testExpectedRackSetupSchema(t *testing.T, dbSession *db.Session) {
	ctx := context.Background()
	// create User table
	err := dbSession.DB.ResetModel(ctx, (*User)(nil))
	assert.Nil(t, err)
	// create InfrastructureProvider table
	err = dbSession.DB.ResetModel(ctx, (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(ctx, (*Site)(nil))
	assert.Nil(t, err)
	// create ExpectedRack table
	err = dbSession.DB.ResetModel(ctx, (*ExpectedRack)(nil))
	assert.Nil(t, err)

	// Add deferrable unique constraint for (rack_id, site_id) combination
	// This constraint is defined in migration 20260429100000_expected_rack.go
	// and enforces that rack_id is unique within a site (but may repeat across sites).
	_, err = dbSession.DB.Exec("ALTER TABLE expected_rack DROP CONSTRAINT IF EXISTS expected_rack_rack_id_site_id_key")
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("ALTER TABLE expected_rack ADD CONSTRAINT expected_rack_rack_id_site_id_key UNIQUE (rack_id, site_id) DEFERRABLE INITIALLY DEFERRED")
	assert.Nil(t, err)
}

func TestExpectedRackDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedRackSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	ip2 := TestBuildInfrastructureProvider(t, dbSession, "test-provider-2", "test-org-2", user)
	otherSite := TestBuildSite(t, dbSession, ip2, "test-site-2", user)

	erd := NewExpectedRackDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []ExpectedRackCreateInput
		expectError        bool
		errorContains      string
		verifyChildSpanner bool
	}{
		{
			desc: "create one with populated labels",
			inputs: []ExpectedRackCreateInput{
				{
					ExpectedRackID: uuid.New(),
					RackID:         "rack-001",
					SiteID:         site.ID,
					RackProfileID:  "profile-1",
					Name:           "Rack 1",
					Description:    "First rack",
					Labels: map[string]string{
						"environment": "test",
						"location":    "datacenter1",
					},
					CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create one with empty labels (defaults)",
			inputs: []ExpectedRackCreateInput{
				{
					ExpectedRackID: uuid.New(),
					RackID:         "rack-002",
					SiteID:         site.ID,
					RackProfileID:  "profile-2",
					CreatedBy:      user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "create same rack_id on a different site succeeds",
			inputs: []ExpectedRackCreateInput{
				{
					ExpectedRackID: uuid.New(),
					RackID:         "rack-001",
					SiteID:         otherSite.ID,
					RackProfileID:  "profile-1",
					CreatedBy:      user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "fail to create duplicate (rack_id, site_id) pair",
			inputs: []ExpectedRackCreateInput{
				{
					ExpectedRackID: uuid.New(),
					RackID:         "rack-001",
					SiteID:         site.ID,
					RackProfileID:  "profile-1",
					CreatedBy:      user.ID,
				},
			},
			expectError:   true,
			errorContains: "duplicate key",
		},
		{
			desc: "fail to create with non-existent site",
			inputs: []ExpectedRackCreateInput{
				{
					ExpectedRackID: uuid.New(),
					RackID:         "rack-003",
					SiteID:         uuid.New(),
					RackProfileID:  "profile-3",
					CreatedBy:      user.ID,
				},
			},
			expectError:   true,
			errorContains: "violates foreign key constraint",
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, input := range tc.inputs {
				er, err := erd.Create(ctx, nil, input)
				if err != nil {
					assert.True(t, tc.expectError, "Expected error but got none")
					assert.Nil(t, er)
					if tc.errorContains != "" {
						assert.Contains(t, err.Error(), tc.errorContains, "Error should contain expected substring")
					}
				} else {
					assert.False(t, tc.expectError, "Expected success but got error: %v", err)
					assert.NotNil(t, er)
					assert.Equal(t, input.ExpectedRackID, er.ID)
					assert.Equal(t, input.RackID, er.RackID)
					assert.Equal(t, input.SiteID, er.SiteID)
					assert.Equal(t, input.RackProfileID, er.RackProfileID)
					assert.Equal(t, input.Name, er.Name)
					assert.Equal(t, input.Description, er.Description)
					if input.Labels == nil {
						// default is empty map
						assert.Equal(t, Labels{}, er.Labels)
					} else {
						assert.Equal(t, Labels(input.Labels), er.Labels)
					}
				}

				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}

				if err != nil {
					t.Logf("%s", err.Error())
					return
				}
			}
		})
	}
}

func testExpectedRackDAOCreateExpectedRacks(ctx context.Context, t *testing.T, dbSession *db.Session) (created []ExpectedRack, site *Site, otherSite *Site, user *User) {
	// Create test dependencies
	user = TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site = TestBuildSite(t, dbSession, ip, "test-site", user)

	ip2 := TestBuildInfrastructureProvider(t, dbSession, "test-provider-2", "test-org-2", user)
	otherSite = TestBuildSite(t, dbSession, ip2, "test-site-2", user)

	createInputs := []ExpectedRackCreateInput{
		{
			ExpectedRackID: uuid.New(),
			RackID:         "rack-001",
			SiteID:         site.ID,
			RackProfileID:  "profile-A",
			Name:           "Rack One",
			Description:    "First rack",
			Labels: map[string]string{
				"environment": "test",
				"location":    "datacenter1",
			},
			CreatedBy: user.ID,
		},
		{
			ExpectedRackID: uuid.New(),
			RackID:         "rack-002",
			SiteID:         site.ID,
			RackProfileID:  "profile-B",
			Name:           "Rack Two",
			Description:    "Second rack",
			Labels: map[string]string{
				"environment": "production",
			},
			CreatedBy: user.ID,
		},
		{
			ExpectedRackID: uuid.New(),
			RackID:         "rack-003",
			SiteID:         otherSite.ID,
			RackProfileID:  "profile-A",
			Name:           "Rack Three",
			CreatedBy:      user.ID,
		},
	}

	erd := NewExpectedRackDAO(dbSession)

	for _, input := range createInputs {
		erCre, err := erd.Create(ctx, nil, input)
		assert.Nil(t, err)
		assert.NotNil(t, erCre)
		created = append(created, *erCre)
	}

	return
}

func TestExpectedRackDAO_Get(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedRackSetupSchema(t, dbSession)

	erExp, _, _, _ := testExpectedRackDAOCreateExpectedRacks(ctx, t, dbSession)
	erd := NewExpectedRackDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		expectedRackID     uuid.UUID
		expectError        bool
		expectedErrVal     error
		expected           *ExpectedRack
		verifyChildSpanner bool
	}{
		{
			desc:               "Get success when ExpectedRack exists [0]",
			expectedRackID:     erExp[0].ID,
			expectError:        false,
			expected:           &erExp[0],
			verifyChildSpanner: true,
		},
		{
			desc:           "Get success when ExpectedRack exists [1]",
			expectedRackID: erExp[1].ID,
			expectError:    false,
			expected:       &erExp[1],
		},
		{
			desc:           "Get returns ErrDoesNotExist when not found",
			expectedRackID: uuid.New(),
			expectError:    true,
			expectedErrVal: db.ErrDoesNotExist,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := erd.Get(ctx, nil, tc.expectedRackID, nil, false)
			assert.Equal(t, tc.expectError, err != nil)
			if tc.expectError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.Equal(t, tc.expected.ID, tmp.ID)
				assert.Equal(t, tc.expected.RackID, tmp.RackID)
				assert.Equal(t, tc.expected.SiteID, tmp.SiteID)
				assert.Equal(t, tc.expected.RackProfileID, tmp.RackProfileID)
				assert.Equal(t, tc.expected.Name, tmp.Name)
				assert.Equal(t, tc.expected.Description, tmp.Description)
				assert.Equal(t, tc.expected.Labels, tmp.Labels)
			} else {
				t.Logf("%s", err.Error())
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestExpectedRackDAO_Get_includeRelations(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedRackSetupSchema(t, dbSession)

	created, _, _, _ := testExpectedRackDAOCreateExpectedRacks(ctx, t, dbSession)
	erd := NewExpectedRackDAO(dbSession)

	got, err := erd.Get(ctx, nil, created[0].ID, []string{SiteRelationName}, false)
	assert.NoError(t, err)
	assert.NotNil(t, got)
	assert.NotNil(t, got.Site)
}

func TestExpectedRackDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedRackSetupSchema(t, dbSession)

	erd := NewExpectedRackDAO(dbSession)

	// Empty case before inserting test data
	t.Run("GetAll empty returns no objects", func(t *testing.T) {
		got, total, err := erd.GetAll(ctx, nil, ExpectedRackFilterInput{}, paginator.PageInput{}, nil)
		assert.Nil(t, err)
		assert.Equal(t, 0, len(got))
		assert.Equal(t, 0, total)
	})

	// Create test data
	created, site, otherSite, _ := testExpectedRackDAOCreateExpectedRacks(ctx, t, dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		filter             ExpectedRackFilterInput
		pageInput          paginator.PageInput
		expectedCount      int
		expectedTotal      *int
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters returns all objects",
			expectedCount:      3,
			expectedTotal:      cutil.GetPtr(3),
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "GetAll with single SiteID filter returns scoped objects",
			filter: ExpectedRackFilterInput{
				SiteIDs: []uuid.UUID{site.ID},
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(2),
			expectedError: false,
		},
		{
			desc: "GetAll with otherSite SiteID filter returns one object",
			filter: ExpectedRackFilterInput{
				SiteIDs: []uuid.UUID{otherSite.ID},
			},
			expectedCount: 1,
			expectedTotal: cutil.GetPtr(1),
			expectedError: false,
		},
		{
			desc: "GetAll with RackProfileIDs filter returns objects",
			filter: ExpectedRackFilterInput{
				RackProfileIDs: []string{"profile-A"},
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(2),
			expectedError: false,
		},
		{
			desc: "GetAll with RackIDs filter returns objects",
			filter: ExpectedRackFilterInput{
				RackIDs: []string{created[0].RackID, created[2].RackID},
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(2),
			expectedError: false,
		},
		{
			desc: "GetAll with empty RackIDs filter returns no objects",
			filter: ExpectedRackFilterInput{
				RackIDs: []string{},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with ExpectedRackIDs filter returns objects",
			filter: ExpectedRackFilterInput{
				ExpectedRackIDs: []uuid.UUID{created[0].ID, created[2].ID},
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(2),
			expectedError: false,
		},
		{
			desc: "GetAll with empty ExpectedRackIDs filter returns no objects",
			filter: ExpectedRackFilterInput{
				ExpectedRackIDs: []uuid.UUID{},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with search query filter returns objects",
			filter: ExpectedRackFilterInput{
				SearchQuery: cutil.GetPtr("rack-001"),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			// Search uses an OR-style tokenization (space-separated tokens
			// become `|` operands), so we use a single token unique to one
			// fixture's name to scope the match to one record.
			desc: "GetAll with search query matching name",
			filter: ExpectedRackFilterInput{
				SearchQuery: cutil.GetPtr("Two"),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with limit returns subset",
			pageInput: paginator.PageInput{
				Offset: cutil.GetPtr(0),
				Limit:  cutil.GetPtr(2),
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc: "GetAll with offset returns subset",
			pageInput: paginator.PageInput{
				Offset: cutil.GetPtr(1),
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc: "GetAll with order by created descending returns objects",
			pageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "created",
					Order: paginator.OrderDescending,
				},
			},
			expectedCount: 3,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc: "GetAll with order by name ascending returns objects",
			pageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "name",
					Order: paginator.OrderAscending,
				},
			},
			expectedCount: 3,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := erd.GetAll(ctx, nil, tc.filter, tc.pageInput, nil)
			if err != nil {
				t.Logf("%s", err.Error())
			}

			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				assert.Equal(t, tc.expectedCount, len(got))
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestExpectedRackDAO_GetAll_includeRelations(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedRackSetupSchema(t, dbSession)

	erd := NewExpectedRackDAO(dbSession)
	_, _, _, _ = testExpectedRackDAOCreateExpectedRacks(ctx, t, dbSession)

	got, _, err := erd.GetAll(ctx, nil, ExpectedRackFilterInput{}, paginator.PageInput{}, []string{SiteRelationName})
	assert.NoError(t, err)
	assert.Equal(t, 3, len(got))

	for _, er := range got {
		assert.NotNil(t, er.Site)
	}
}

func TestExpectedRackDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedRackSetupSchema(t, dbSession)

	erExp, _, _, _ := testExpectedRackDAOCreateExpectedRacks(ctx, t, dbSession)
	erd := NewExpectedRackDAO(dbSession)
	assert.NotNil(t, erd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		input              ExpectedRackUpdateInput
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc: "Update RackProfileID",
			input: ExpectedRackUpdateInput{
				ExpectedRackID: erExp[0].ID,
				RackProfileID:  cutil.GetPtr("profile-Z"),
			},
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "Update RackID (rename)",
			input: ExpectedRackUpdateInput{
				ExpectedRackID: erExp[2].ID,
				RackID:         cutil.GetPtr("rack-003-renamed"),
			},
			expectedError: false,
		},
		{
			desc: "Update Name",
			input: ExpectedRackUpdateInput{
				ExpectedRackID: erExp[1].ID,
				Name:           cutil.GetPtr("Updated Rack Two"),
			},
			expectedError: false,
		},
		{
			desc: "Update Description",
			input: ExpectedRackUpdateInput{
				ExpectedRackID: erExp[2].ID,
				Description:    cutil.GetPtr("Updated description"),
			},
			expectedError: false,
		},
		{
			desc: "Update Labels (populated map)",
			input: ExpectedRackUpdateInput{
				ExpectedRackID: erExp[0].ID,
				Labels: map[string]string{
					"environment": "staging",
					"owner":       "team-alpha",
				},
			},
			expectedError: false,
		},
		{
			desc: "Update multiple fields at once",
			input: ExpectedRackUpdateInput{
				ExpectedRackID: erExp[1].ID,
				RackProfileID:  cutil.GetPtr("profile-X"),
				Name:           cutil.GetPtr("Final Rack Two"),
				Description:    cutil.GetPtr("New desc"),
				Labels: map[string]string{
					"team": "ops",
				},
			},
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := erd.Update(ctx, nil, tc.input)
			assert.Equal(t, tc.expectedError, err != nil)
			if err != nil {
				t.Logf("%s", err.Error())
			}
			if !tc.expectedError {
				assert.Nil(t, err)
				assert.NotNil(t, got)
				if tc.input.RackID != nil {
					assert.Equal(t, *tc.input.RackID, got.RackID)
				}
				if tc.input.RackProfileID != nil {
					assert.Equal(t, *tc.input.RackProfileID, got.RackProfileID)
				}
				if tc.input.Name != nil {
					assert.Equal(t, *tc.input.Name, got.Name)
				}
				if tc.input.Description != nil {
					assert.Equal(t, *tc.input.Description, got.Description)
				}
				if tc.input.Labels != nil {
					assert.Equal(t, Labels(tc.input.Labels), got.Labels)
				}

				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestExpectedRackDAO_Update_NotFound(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedRackSetupSchema(t, dbSession)

	_, _, _, _ = testExpectedRackDAOCreateExpectedRacks(ctx, t, dbSession)
	erd := NewExpectedRackDAO(dbSession)

	// Updating a non-existent rack should result in a count mismatch error from the
	// post-update SELECT.
	_, err := erd.Update(ctx, nil, ExpectedRackUpdateInput{
		ExpectedRackID: uuid.New(),
		Name:           cutil.GetPtr("Should not exist"),
	})
	assert.Error(t, err)
}

func TestExpectedRackDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedRackSetupSchema(t, dbSession)

	erExp, _, _, _ := testExpectedRackDAOCreateExpectedRacks(ctx, t, dbSession)
	erd := NewExpectedRackDAO(dbSession)
	assert.NotNil(t, erd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		expectedRackID     uuid.UUID
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			desc:               "delete existing object success",
			expectedRackID:     erExp[1].ID,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			desc:           "delete non-existent object success",
			expectedRackID: uuid.New(),
			wantErr:        false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := erd.Delete(ctx, nil, tc.expectedRackID)

			if tc.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)

			var res ExpectedRack
			err = dbSession.DB.NewSelect().Model(&res).Where("er.id = ?", tc.expectedRackID).Scan(ctx)
			assert.ErrorIs(t, err, sql.ErrNoRows)

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestExpectedRackDAO_DeleteAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedRackSetupSchema(t, dbSession)

	erd := NewExpectedRackDAO(dbSession)

	t.Run("DeleteAll scoped by SiteIDs", func(t *testing.T) {
		_, site, otherSite, _ := testExpectedRackDAOCreateExpectedRacks(ctx, t, dbSession)

		err := erd.DeleteAll(ctx, nil, ExpectedRackFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		})
		assert.NoError(t, err)

		// Site rows are gone
		got, total, err := erd.GetAll(ctx, nil, ExpectedRackFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		}, paginator.PageInput{}, nil)
		assert.NoError(t, err)
		assert.Equal(t, 0, len(got))
		assert.Equal(t, 0, total)

		// Other site is unaffected
		got, total, err = erd.GetAll(ctx, nil, ExpectedRackFilterInput{
			SiteIDs: []uuid.UUID{otherSite.ID},
		}, paginator.PageInput{}, nil)
		assert.NoError(t, err)
		assert.Equal(t, 1, len(got))
		assert.Equal(t, 1, total)
	})

	// Reset for the unscoped case
	testExpectedRackSetupSchema(t, dbSession)

	t.Run("DeleteAll without filter is rejected", func(t *testing.T) {
		expectedRacks, _, _, _ := testExpectedRackDAOCreateExpectedRacks(ctx, t, dbSession)

		err := erd.DeleteAll(ctx, nil, ExpectedRackFilterInput{})
		assert.ErrorIs(t, err, db.ErrInvalidParams)

		// Records must be untouched after the rejection.
		got, total, err := erd.GetAll(ctx, nil, ExpectedRackFilterInput{}, paginator.PageInput{}, nil)
		assert.NoError(t, err)
		assert.Equal(t, len(expectedRacks), len(got))
		assert.Equal(t, len(expectedRacks), total)
	})
}

func TestExpectedRackDAO_ReplaceAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedRackSetupSchema(t, dbSession)

	erd := NewExpectedRackDAO(dbSession)

	// First: replace empty (no existing rows) with a new set scoped to a site.
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	ip2 := TestBuildInfrastructureProvider(t, dbSession, "test-provider-2", "test-org-2", user)
	otherSite := TestBuildSite(t, dbSession, ip2, "test-site-2", user)

	// Pre-populate otherSite with rows that should not be touched by ReplaceAll(filter site).
	preExisting := []ExpectedRackCreateInput{
		{
			ExpectedRackID: uuid.New(),
			RackID:         "rack-other-1",
			SiteID:         otherSite.ID,
			RackProfileID:  "other-profile",
			Name:           "Untouched Rack",
			CreatedBy:      user.ID,
		},
	}
	pre, err := erd.CreateMultiple(ctx, nil, preExisting)
	assert.Nil(t, err)
	assert.Equal(t, 1, len(pre))

	t.Run("ReplaceAll into empty scope (site) inserts new objects", func(t *testing.T) {
		newInputs := []ExpectedRackCreateInput{
			{
				ExpectedRackID: uuid.New(),
				RackID:         "rack-A",
				SiteID:         site.ID,
				RackProfileID:  "profile-A",
				Name:           "Replace A",
				Labels: map[string]string{
					"new": "true",
				},
				CreatedBy: user.ID,
			},
			{
				ExpectedRackID: uuid.New(),
				RackID:         "rack-B",
				SiteID:         site.ID,
				RackProfileID:  "profile-B",
				Name:           "Replace B",
				CreatedBy:      user.ID,
			},
		}

		result, err := erd.ReplaceAll(ctx, nil, ExpectedRackFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		}, newInputs)
		assert.NoError(t, err)
		assert.Equal(t, 2, len(result))

		// Site has the new objects
		got, total, err := erd.GetAll(ctx, nil, ExpectedRackFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		}, paginator.PageInput{}, nil)
		assert.NoError(t, err)
		assert.Equal(t, 2, len(got))
		assert.Equal(t, 2, total)

		// otherSite preexisting row is still there
		got, total, err = erd.GetAll(ctx, nil, ExpectedRackFilterInput{
			SiteIDs: []uuid.UUID{otherSite.ID},
		}, paginator.PageInput{}, nil)
		assert.NoError(t, err)
		assert.Equal(t, 1, len(got))
		assert.Equal(t, 1, total)
	})

	t.Run("ReplaceAll over existing scope (site) swaps content", func(t *testing.T) {
		newInputs := []ExpectedRackCreateInput{
			{
				ExpectedRackID: uuid.New(),
				RackID:         "rack-C",
				SiteID:         site.ID,
				RackProfileID:  "profile-C",
				Name:           "Replace C",
				CreatedBy:      user.ID,
			},
		}

		result, err := erd.ReplaceAll(ctx, nil, ExpectedRackFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		}, newInputs)
		assert.NoError(t, err)
		assert.Equal(t, 1, len(result))
		assert.Equal(t, "rack-C", result[0].RackID)

		// Old rows for site are gone
		got, total, err := erd.GetAll(ctx, nil, ExpectedRackFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		}, paginator.PageInput{}, nil)
		assert.NoError(t, err)
		assert.Equal(t, 1, len(got))
		assert.Equal(t, 1, total)
		assert.Equal(t, "rack-C", got[0].RackID)

		// otherSite still untouched
		got, _, err = erd.GetAll(ctx, nil, ExpectedRackFilterInput{
			SiteIDs: []uuid.UUID{otherSite.ID},
		}, paginator.PageInput{}, nil)
		assert.NoError(t, err)
		assert.Equal(t, 1, len(got))
	})

	t.Run("ReplaceAll with empty inputs deletes scope", func(t *testing.T) {
		result, err := erd.ReplaceAll(ctx, nil, ExpectedRackFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		}, nil)
		assert.NoError(t, err)
		assert.Equal(t, 0, len(result))

		got, total, err := erd.GetAll(ctx, nil, ExpectedRackFilterInput{
			SiteIDs: []uuid.UUID{site.ID},
		}, paginator.PageInput{}, nil)
		assert.NoError(t, err)
		assert.Equal(t, 0, len(got))
		assert.Equal(t, 0, total)
	})
}
