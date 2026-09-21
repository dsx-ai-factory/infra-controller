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

func TestExpectedPowerShelf_FromProto(t *testing.T) {
	id := uuid.New()
	rackID := "rack-1"
	name := "ps-1"
	manufacturer := "ACME"
	model := "PS1"
	description := "primary"
	var slot, trayIdx, host int32 = 1, 2, 3

	t.Run("nil proto leaves receiver unchanged", func(t *testing.T) {
		eps := &ExpectedPowerShelf{ID: id, BmcMacAddress: "aa:bb"}
		eps.FromProto(nil)

		assert.Equal(t, id, eps.ID)
		assert.Equal(t, "aa:bb", eps.BmcMacAddress)
	})

	t.Run("invalid id leaves eps.ID unchanged", func(t *testing.T) {
		eps := &ExpectedPowerShelf{ID: id}
		eps.FromProto(&corev1.ExpectedPowerShelf{
			ExpectedPowerShelfId: &corev1.UUID{Value: "not-a-uuid"},
			BmcMacAddress:        "aa:bb",
		})

		assert.Equal(t, id, eps.ID)
		assert.Equal(t, "aa:bb", eps.BmcMacAddress)
	})

	t.Run("populates all proto fields", func(t *testing.T) {
		eps := &ExpectedPowerShelf{}
		eps.FromProto(&corev1.ExpectedPowerShelf{
			ExpectedPowerShelfId: &corev1.UUID{Value: id.String()},
			BmcMacAddress:        "aa:bb:cc:dd:ee:ff",
			ShelfSerialNumber:    "SSN-1",
			BmcIpAddress:         "10.0.0.1",
			RackId:               &corev1.RackId{Id: rackID},
			Name:                 &name,
			Manufacturer:         &manufacturer,
			Model:                &model,
			Description:          &description,
			SlotId:               &slot,
			TrayIdx:              &trayIdx,
			HostId:               &host,
			Metadata: &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "env", Value: cutil.GetPtr("prod")},
				},
			},
		})

		assert.Equal(t, id, eps.ID)
		assert.Equal(t, "aa:bb:cc:dd:ee:ff", eps.BmcMacAddress)
		assert.Equal(t, "SSN-1", eps.ShelfSerialNumber)
		if assert.NotNil(t, eps.BmcIpAddress) {
			assert.Equal(t, "10.0.0.1", *eps.BmcIpAddress)
		}
		if assert.NotNil(t, eps.RackID) {
			assert.Equal(t, rackID, *eps.RackID)
		}
		assert.Equal(t, &name, eps.Name)
		assert.Equal(t, &manufacturer, eps.Manufacturer)
		assert.Equal(t, &model, eps.Model)
		assert.Equal(t, &description, eps.Description)
		assert.Equal(t, &slot, eps.SlotID)
		assert.Equal(t, &trayIdx, eps.TrayIdx)
		assert.Equal(t, &host, eps.HostID)
		assert.Equal(t, Labels{"env": "prod"}, eps.Labels)
	})

	t.Run("empty BmcIpAddress yields nil pointer", func(t *testing.T) {
		eps := &ExpectedPowerShelf{BmcIpAddress: cutil.GetPtr("stale")}
		eps.FromProto(&corev1.ExpectedPowerShelf{
			ExpectedPowerShelfId: &corev1.UUID{Value: id.String()},
			BmcIpAddress:         "",
		})

		assert.Nil(t, eps.BmcIpAddress)
	})

	t.Run("nil RackId clears eps.RackID", func(t *testing.T) {
		stale := "stale-rack"
		eps := &ExpectedPowerShelf{RackID: &stale}
		eps.FromProto(&corev1.ExpectedPowerShelf{
			ExpectedPowerShelfId: &corev1.UUID{Value: id.String()},
			BmcMacAddress:        "aa:bb",
		})

		assert.Nil(t, eps.RackID)
	})
}

// reset the tables needed for ExpectedPowerShelf tests
func testExpectedPowerShelfSetupSchema(t *testing.T, dbSession *db.Session) {
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
	// create ExpectedPowerShelf table
	err = dbSession.DB.ResetModel(ctx, (*ExpectedPowerShelf)(nil))
	assert.Nil(t, err)
}

func TestExpectedPowerShelfSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedPowerShelfSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	epsd := NewExpectedPowerShelfDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []ExpectedPowerShelfCreateInput
		expectError        bool
		errorContains      string
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			inputs: []ExpectedPowerShelfCreateInput{
				{
					ExpectedPowerShelfID: uuid.New(),
					SiteID:               site.ID,
					BmcMacAddress:        "00:1B:44:11:3A:B7",
					ShelfSerialNumber:    "SHELF123",
					BmcIpAddress:         cutil.GetPtr("192.168.1.100"),
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
			desc: "create multiple, some with nullable fields",
			inputs: []ExpectedPowerShelfCreateInput{
				{
					ExpectedPowerShelfID: uuid.New(),
					SiteID:               site.ID,
					BmcMacAddress:        "00:1B:44:11:3A:B8",
					ShelfSerialNumber:    "SHELF789",
					BmcIpAddress:         cutil.GetPtr("10.0.0.1"),
					Labels: map[string]string{
						"environment": "production",
					},
					CreatedBy: user.ID,
				},
				{
					ExpectedPowerShelfID: uuid.New(),
					SiteID:               site.ID,
					BmcMacAddress:        "00:1B:44:11:3A:B9",
					ShelfSerialNumber:    "SHELF456",
					BmcIpAddress:         nil,
					Labels:               nil,
					CreatedBy:            user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "fail to create with non-existent site",
			inputs: []ExpectedPowerShelfCreateInput{
				{
					ExpectedPowerShelfID: uuid.New(),
					SiteID:               uuid.New(),
					BmcMacAddress:        "00:1B:44:11:3A:C0",
					ShelfSerialNumber:    "SHELF-NOSITE",
					CreatedBy:            user.ID,
				},
			},
			expectError:   true,
			errorContains: "violates foreign key constraint",
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, input := range tc.inputs {
				eps, err := epsd.Create(ctx, nil, input)
				if err != nil {
					assert.True(t, tc.expectError, "Expected error but got none")
					assert.Nil(t, eps)
					if tc.errorContains != "" {
						assert.Contains(t, err.Error(), tc.errorContains, "Error should contain expected substring")
					}
				} else {
					assert.False(t, tc.expectError, "Expected success but got error: %v", err)
					assert.NotNil(t, eps)
					assert.Equal(t, input.BmcMacAddress, eps.BmcMacAddress)
					assert.Equal(t, input.ShelfSerialNumber, eps.ShelfSerialNumber)
					assert.Equal(t, input.BmcIpAddress, eps.BmcIpAddress)
					assert.Equal(t, Labels(input.Labels), eps.Labels)
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

func testExpectedPowerShelfSQLDAOCreateExpectedPowerShelves(ctx context.Context, t *testing.T, dbSession *db.Session) (created []ExpectedPowerShelf) {
	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	var createInputs []ExpectedPowerShelfCreateInput
	{
		// ExpectedPowerShelf set 1
		createInputs = append(createInputs, ExpectedPowerShelfCreateInput{
			ExpectedPowerShelfID: uuid.New(),
			SiteID:               site.ID,
			BmcMacAddress:        "00:1B:44:11:3A:B7",
			ShelfSerialNumber:    "SHELF123",
			BmcIpAddress:         cutil.GetPtr("192.168.1.100"),
			Labels: map[string]string{
				"environment": "test",
				"location":    "datacenter1",
			},
			CreatedBy: user.ID,
		})

		// ExpectedPowerShelf set 2
		createInputs = append(createInputs, ExpectedPowerShelfCreateInput{
			ExpectedPowerShelfID: uuid.New(),
			SiteID:               site.ID,
			BmcMacAddress:        "00:1B:44:11:3A:B8",
			ShelfSerialNumber:    "SHELF789",
			BmcIpAddress:         cutil.GetPtr("10.0.0.1"),
			Labels: map[string]string{
				"environment": "production",
			},
			CreatedBy: user.ID,
		})

		// ExpectedPowerShelf set 3
		createInputs = append(createInputs, ExpectedPowerShelfCreateInput{
			ExpectedPowerShelfID: uuid.New(),
			SiteID:               site.ID,
			BmcMacAddress:        "00:1B:44:11:3A:B9",
			ShelfSerialNumber:    "SHELF456",
			BmcIpAddress:         nil,
			Labels:               nil,
			CreatedBy:            user.ID,
		})
	}

	epsd := NewExpectedPowerShelfDAO(dbSession)

	// ExpectedPowerShelf created
	for _, input := range createInputs {
		epsCre, _ := epsd.Create(ctx, nil, input)
		assert.NotNil(t, epsCre)
		created = append(created, *epsCre)
	}

	return
}

func TestExpectedPowerShelfSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedPowerShelfSetupSchema(t, dbSession)

	epsExp := testExpectedPowerShelfSQLDAOCreateExpectedPowerShelves(ctx, t, dbSession)
	epsd := NewExpectedPowerShelfDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		eps                ExpectedPowerShelf
		expectError        bool
		expectedErrVal     error
		verifyChildSpanner bool
	}{
		{
			desc:               "GetById success when ExpectedPowerShelf exists on [0]",
			eps:                epsExp[0],
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:        "GetById success when ExpectedPowerShelf exists on [1]",
			eps:         epsExp[1],
			expectError: false,
		},
		{
			desc: "GetById success when ExpectedPowerShelf not found",
			eps: ExpectedPowerShelf{
				ID: uuid.New(),
			},
			expectError:    true,
			expectedErrVal: db.ErrDoesNotExist,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := epsd.Get(ctx, nil, tc.eps.ID, nil, false)
			assert.Equal(t, tc.expectError, err != nil)
			if tc.expectError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.Equal(t, tc.eps.ID, tmp.ID)
				assert.Equal(t, tc.eps.BmcMacAddress, tmp.BmcMacAddress)
				assert.Equal(t, tc.eps.ShelfSerialNumber, tmp.ShelfSerialNumber)
				assert.Equal(t, tc.eps.BmcIpAddress, tmp.BmcIpAddress)
				assert.Equal(t, tc.eps.Labels, tmp.Labels)
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

func TestExpectedPowerShelfSQLDAO_Get_includeRelations(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedPowerShelfSetupSchema(t, dbSession)

	created := testExpectedPowerShelfSQLDAOCreateExpectedPowerShelves(ctx, t, dbSession)
	req := NewExpectedPowerShelfDAO(dbSession)

	got, err := req.Get(ctx, nil, created[1].ID, []string{SiteRelationName}, false)
	assert.NoError(t, err)
	assert.NotNil(t, got)
	assert.NotNil(t, got.Site)
}

func TestExpectedPowerShelfSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedPowerShelfSetupSchema(t, dbSession)

	epsd := NewExpectedPowerShelfDAO(dbSession)

	// Create test data
	created := testExpectedPowerShelfSQLDAOCreateExpectedPowerShelves(ctx, t, dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		filter             ExpectedPowerShelfFilterInput
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
			desc: "GetAll with SiteIDs filter returns objects",
			filter: ExpectedPowerShelfFilterInput{
				SiteIDs: []uuid.UUID{created[0].SiteID},
			},
			expectedCount: 3,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc: "GetAll with BmcMacAddress filter returns objects",
			filter: ExpectedPowerShelfFilterInput{
				BmcMacAddresses: []string{created[0].BmcMacAddress},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with ShelfSerialNumber filter returns objects",
			filter: ExpectedPowerShelfFilterInput{
				ShelfSerialNumbers: []string{created[0].ShelfSerialNumber},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with search query filter returns objects",
			filter: ExpectedPowerShelfFilterInput{
				SearchQuery: cutil.GetPtr("SHELF123"),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with ExpectedPowerShelfIDs filter returns objects",
			filter: ExpectedPowerShelfFilterInput{
				ExpectedPowerShelfIDs: []uuid.UUID{created[0].ID, created[2].ID},
			},
			expectedCount: 2,
			expectedError: false,
		},
		{
			desc: "GetAll with limit returns objects",
			pageInput: paginator.PageInput{
				Offset: cutil.GetPtr(0),
				Limit:  cutil.GetPtr(2),
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc: "GetAll with offset returns objects",
			pageInput: paginator.PageInput{
				Offset: cutil.GetPtr(1),
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc: "GetAll with order by returns objects",
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
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := epsd.GetAll(ctx, nil, tc.filter, tc.pageInput, nil)
			if err != nil {
				t.Logf("%s", err.Error())
			}

			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
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

func TestExpectedPowerShelfSQLDAO_GetAll_includeRelations(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedPowerShelfSetupSchema(t, dbSession)

	req := NewExpectedPowerShelfDAO(dbSession)
	_ = testExpectedPowerShelfSQLDAOCreateExpectedPowerShelves(ctx, t, dbSession)

	got, _, err := req.GetAll(ctx, nil, ExpectedPowerShelfFilterInput{}, paginator.PageInput{}, []string{SiteRelationName})
	assert.NoError(t, err)
	assert.Equal(t, 3, len(got))

	for _, eps := range got {
		assert.NotNil(t, eps.Site)
	}
}

func TestExpectedPowerShelfSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedPowerShelfSetupSchema(t, dbSession)

	epsExp := testExpectedPowerShelfSQLDAOCreateExpectedPowerShelves(ctx, t, dbSession)
	epsd := NewExpectedPowerShelfDAO(dbSession)
	assert.NotNil(t, epsd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		input              ExpectedPowerShelfUpdateInput
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc: "Update BMC MAC address",
			input: ExpectedPowerShelfUpdateInput{
				ExpectedPowerShelfID: epsExp[0].ID,
				BmcMacAddress:        cutil.GetPtr("00:1B:44:11:3A:C1"),
			},
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "Update shelf serial number",
			input: ExpectedPowerShelfUpdateInput{
				ExpectedPowerShelfID: epsExp[1].ID,
				ShelfSerialNumber:    cutil.GetPtr("NEWSHELF789"),
			},
			expectedError: false,
		},
		{
			desc: "Update IP address",
			input: ExpectedPowerShelfUpdateInput{
				ExpectedPowerShelfID: epsExp[2].ID,
				BmcIpAddress:         cutil.GetPtr("172.16.0.1"),
			},
			expectedError: false,
		},
		{
			desc: "Update labels",
			input: ExpectedPowerShelfUpdateInput{
				ExpectedPowerShelfID: epsExp[0].ID,
				Labels: map[string]string{
					"environment": "staging",
					"owner":       "team-alpha",
				},
			},
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := epsd.Update(ctx, nil, tc.input)
			assert.Equal(t, tc.expectedError, err != nil)
			if err != nil {
				t.Logf("%s", err.Error())
			}
			if !tc.expectedError {
				assert.Nil(t, err)
				assert.NotNil(t, got)
				if tc.input.BmcMacAddress != nil {
					assert.Equal(t, *tc.input.BmcMacAddress, got.BmcMacAddress)
				}
				if tc.input.ShelfSerialNumber != nil {
					assert.Equal(t, *tc.input.ShelfSerialNumber, got.ShelfSerialNumber)
				}
				if tc.input.BmcIpAddress != nil {
					assert.Equal(t, tc.input.BmcIpAddress, got.BmcIpAddress)
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

func TestExpectedPowerShelfSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedPowerShelfSetupSchema(t, dbSession)

	epsExp := testExpectedPowerShelfSQLDAOCreateExpectedPowerShelves(ctx, t, dbSession)
	epsd := NewExpectedPowerShelfDAO(dbSession)
	assert.NotNil(t, epsd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		eps                ExpectedPowerShelf
		input              ExpectedPowerShelfClearInput
		expectedUpdate     bool
		verifyChildSpanner bool
	}{
		{
			desc: "can clear BmcIpAddress",
			eps:  epsExp[1],
			input: ExpectedPowerShelfClearInput{
				BmcIpAddress: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear Labels",
			eps:  epsExp[0],
			input: ExpectedPowerShelfClearInput{
				Labels: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear multiple fields",
			eps:  epsExp[0],
			input: ExpectedPowerShelfClearInput{
				BmcIpAddress: true,
				Labels:       true,
			},
			expectedUpdate: true,
		},
		{
			desc:           "nop when no cleared fields are specified",
			eps:            epsExp[2],
			input:          ExpectedPowerShelfClearInput{},
			expectedUpdate: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tc.input.ExpectedPowerShelfID = tc.eps.ID
			tmp, err := epsd.Clear(ctx, nil, tc.input)
			assert.Nil(t, err)
			assert.NotNil(t, tmp)
			if tc.input.BmcIpAddress {
				assert.Nil(t, tmp.BmcIpAddress)
			}
			if tc.input.Labels {
				assert.Nil(t, tmp.Labels)
			}

			if tc.expectedUpdate {
				assert.True(t, tmp.Updated.After(tc.eps.Updated))
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestExpectedPowerShelfSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedPowerShelfSetupSchema(t, dbSession)

	epsExp := testExpectedPowerShelfSQLDAOCreateExpectedPowerShelves(ctx, t, dbSession)
	epsd := NewExpectedPowerShelfDAO(dbSession)
	assert.NotNil(t, epsd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		epsID              uuid.UUID
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object success",
			epsID:              epsExp[1].ID,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			desc:    "delete non-existent object success",
			epsID:   uuid.New(),
			wantErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := epsd.Delete(ctx, nil, tc.epsID)

			if tc.wantErr {
				assert.Error(t, err)
				return
			}

			var res ExpectedPowerShelf

			err = dbSession.DB.NewSelect().Model(&res).Where("eps.id = ?", tc.epsID).Scan(ctx)
			assert.ErrorIs(t, err, sql.ErrNoRows)

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
