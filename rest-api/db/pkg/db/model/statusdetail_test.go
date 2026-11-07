// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
)

func TestStatusDetailSQLDAO_GetAll(t *testing.T) {
	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create tables
	err := dbSession.DB.ResetModel(context.Background(), (*StatusDetail)(nil))
	assert.NoError(t, err)

	entityID1 := uuid.NewString()
	entityID2 := uuid.NewString()
	totalCount := 30

	sds := []StatusDetail{}

	for i := 0; i < totalCount; i++ {
		entityID := entityID1
		status := VpcStatusPending
		if i%2 == 1 {
			entityID = entityID2
			status = VpcStatusProvisioning
		}

		sd := &StatusDetail{
			ID:       uuid.New(),
			EntityID: entityID,
			Count:    1,
			Status:   status,
			Message:  cutil.GetPtr("test message"),
			Created:  db.GetCurTime().Add(time.Duration(i) * time.Second),
		}

		_, err = dbSession.DB.NewInsert().Model(sd).Exec(context.Background())
		assert.NoError(t, err)
		sds = append(sds, *sd)
	}

	type fields struct {
		dbSession *db.Session
	}

	type args struct {
		ctx       context.Context
		entityids []string
		page      paginator.PageInput
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name                string
		fields              fields
		args                args
		wantCount           int
		wantTotalCount      *int
		wantFirstEntry      *StatusDetail
		firstEntryAttribute *string
		wantErr             bool
		verifyChildSpanner  bool
	}{
		{
			name: "get all Status Details by entity IDs",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       ctx,
				entityids: []string{entityID1, entityID2},
			},
			wantCount:          paginator.DefaultLimit,
			wantTotalCount:     &totalCount,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "get all Status Details by a single entity ID in array",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				entityids: []string{entityID1},
			},
			wantCount:      totalCount / 2,
			wantTotalCount: cutil.GetPtr(totalCount / 2),
			wantErr:        false,
		},
		{
			name: "get all Status Details with empty entity IDs",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				entityids: []string{},
			},
			wantCount:      0,
			wantTotalCount: cutil.GetPtr(0),
			wantErr:        false,
		},
		{
			name: "get all Status Details with limit",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				entityids: []string{entityID1, entityID2},
				page:      paginator.PageInput{Limit: cutil.GetPtr(10)},
			},
			wantCount:      10,
			wantTotalCount: &totalCount,
			wantErr:        false,
		},
		{
			name: "get all Status Details with offset",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				entityids: []string{entityID1, entityID2},
				page:      paginator.PageInput{Offset: cutil.GetPtr(15)},
			},
			wantCount:      15,
			wantTotalCount: &totalCount,
			wantErr:        false,
		},
		{
			name: "get all Status Details ordered by created",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				entityids: []string{entityID1, entityID2},
				page:      paginator.PageInput{OrderBy: &paginator.OrderBy{Field: "created", Order: paginator.OrderDescending}},
			},
			wantCount:           paginator.DefaultLimit,
			wantFirstEntry:      &sds[totalCount-1], // The last entry would have last timestamp, and would appear first in the list
			firstEntryAttribute: cutil.GetPtr("Created"),
			wantErr:             false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			sdDAO := StatusDetailSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			got, total, err := sdDAO.GetAll(tt.args.ctx, nil, StatusDetailFilterInput{EntityIDs: tt.args.entityids}, tt.args.page)
			if tt.wantErr {
				assert.NotNil(t, err)
			} else {
				assert.Equal(t, tt.wantCount, len(got))
			}

			if tt.wantTotalCount != nil {
				assert.Equal(t, *tt.wantTotalCount, total)
			}

			if tt.wantFirstEntry != nil {
				assert.True(t, tt.wantFirstEntry.Created.Equal(got[0].Created))
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestStatusDetailSQLDAO_Create(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}

	type args struct {
		ctx   context.Context
		tx    *db.Tx
		input StatusDetailCreateInput
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create tables
	err := dbSession.DB.ResetModel(context.Background(), (*StatusDetail)(nil))
	if err != nil {
		t.Fatal(err)
	}

	sd := &StatusDetail{
		EntityID: uuid.NewString(),
		Status:   VpcStatusPending,
		Message:  cutil.GetPtr("test message"),
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *StatusDetail
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "create Status Detail from params",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				input: StatusDetailCreateInput{
					EntityID: sd.EntityID,
					Status:   sd.Status,
					Message:  sd.Message,
				},
			},
			want:               sd,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			sdd := StatusDetailSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			nsd, err := sdd.Create(tt.args.ctx, tt.args.tx, tt.args.input)
			if tt.wantErr {
				require.NotNil(t, err)
			} else {
				assert.Nil(t, err)
				require.NotNil(t, nsd)
			}

			assert.Equal(t, tt.want.EntityID, nsd.EntityID)
			assert.Equal(t, tt.want.Status, nsd.Status)
			assert.Equal(t, *tt.want.Message, *nsd.Message)
			assert.Equal(t, 1, nsd.Count)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestStatusDetailSQLDAO_Update(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}

	type args struct {
		ctx   context.Context
		input StatusDetailUpdateInput
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create tables
	err := dbSession.DB.ResetModel(context.Background(), (*StatusDetail)(nil))
	if err != nil {
		t.Fatal(err)
	}

	sd := &StatusDetail{
		ID:       uuid.New(),
		EntityID: uuid.NewString(),
		Count:    1,
		Status:   VpcStatusPending,
		Message:  cutil.GetPtr("test message"),
		Created:  time.Now(),
	}

	_, err = dbSession.DB.NewInsert().Model(sd).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	umessage := "test message updated"

	usd := &StatusDetail{
		EntityID: sd.EntityID,
		Status:   VpcStatusReady,
		Message:  &umessage,
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *StatusDetail
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "update StatusDetail from params",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				input: StatusDetailUpdateInput{
					StatusDetailID: sd.ID,
					Status:         usd.Status,
					Message:        usd.Message,
				},
			},
			want:               usd,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			sddao := StatusDetailSQLDAO{
				dbSession: tt.fields.dbSession,
			}

			got, err := sddao.Update(tt.args.ctx, nil, tt.args.input)
			if tt.wantErr {
				assert.NotNil(t, err)
			} else {
				assert.Nil(t, err)
				require.NotNil(t, got)
			}

			assert.Equal(t, tt.want.EntityID, got.EntityID)
			assert.Equal(t, tt.want.Status, got.Status)
			assert.Equal(t, *tt.want.Message, *got.Message)
			assert.Equal(t, 2, got.Count)
			assert.NotEqual(t, tt.want.Updated.String(), got.Updated.String())

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestStatusDetailSQLDAO_GetRecentByEntityIDs(t *testing.T) {
	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create tables
	err := dbSession.DB.ResetModel(context.Background(), (*StatusDetail)(nil))
	assert.NoError(t, err)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	sdDAO := StatusDetailSQLDAO{
		dbSession: dbSession,
	}

	tests := []struct {
		name               string
		entityIDs          []string
		sdTotalCount       int
		sdRecentCount      int
		expectedTotal      int
		expectedStartCount int
	}{
		{
			name:               "recent with large set of status records",
			entityIDs:          []string{uuid.NewString(), uuid.NewString()},
			sdTotalCount:       30,
			sdRecentCount:      5,
			expectedTotal:      10,
			expectedStartCount: 20,
		},
		{
			name:               "recent with small set of status records",
			entityIDs:          []string{uuid.NewString(), uuid.NewString()},
			sdTotalCount:       8,
			sdRecentCount:      5,
			expectedTotal:      8,
			expectedStartCount: 0,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// pre-test
			expectedMap := map[string][]StatusDetail{}
			for i := 0; i < tt.sdTotalCount; i++ {
				entityID := tt.entityIDs[0]
				status := VpcStatusPending
				if i%2 == 1 {
					entityID = tt.entityIDs[1]
					status = VpcStatusProvisioning
				}

				sd := StatusDetail{
					ID:       uuid.New(),
					EntityID: entityID,
					Count:    1,
					Status:   status,
					Message:  cutil.GetPtr("test message"),
					Created:  db.GetCurTime().Add(time.Duration(i) * time.Second),
				}

				_, err = dbSession.DB.NewInsert().Model(&sd).Exec(context.Background())
				assert.NoError(t, err)

				if i >= tt.expectedStartCount {
					expectedMap[entityID] = append([]StatusDetail{sd}, expectedMap[entityID]...)
				}
			}

			// test
			records, err := sdDAO.GetRecentByEntityIDs(ctx, nil, tt.entityIDs, tt.sdRecentCount)
			assert.NoError(t, err)
			// check expected total
			assert.Equal(t, tt.expectedTotal, len(records))
			// build a map with records for each entity
			actualMap := map[string][]StatusDetail{}
			for _, r := range records {
				actualMap[r.EntityID] = append(actualMap[r.EntityID], r)
			}
			// validate that we only get last added records for each entity and their order is correct
			for _, entityID := range tt.entityIDs {
				expected := expectedMap[entityID]
				actual := actualMap[entityID]
				assert.Equal(t, len(expected), len(actual))
				for i, exp := range expected {
					assert.Equal(t, exp.ID, actual[i].ID)
				}
			}
		})
	}
}

func TestStatusDetailSQLDAO_CreateMultiple(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	err := dbSession.DB.ResetModel(context.Background(), (*StatusDetail)(nil))
	assert.NoError(t, err)

	sdd := NewStatusDetailDAO(dbSession)

	tests := []struct {
		desc          string
		inputs        []StatusDetailCreateInput
		expectError   bool
		expectedCount int
	}{
		{
			desc: "create batch of three status details",
			inputs: []StatusDetailCreateInput{
				{
					EntityID: "entity-1",
					Status:   "Ready",
					Message:  cutil.GetPtr("Entity 1 is ready"),
				},
				{
					EntityID: "entity-2",
					Status:   "Pending",
					Message:  cutil.GetPtr("Entity 2 is pending"),
				},
				{
					EntityID: "entity-3",
					Status:   "Error",
					Message:  cutil.GetPtr("Entity 3 has error"),
				},
			},
			expectError:   false,
			expectedCount: 3,
		},
		{
			desc:          "create batch with empty input",
			inputs:        []StatusDetailCreateInput{},
			expectError:   false,
			expectedCount: 0,
		},
		{
			desc: "create batch with single status detail",
			inputs: []StatusDetailCreateInput{
				{
					EntityID: "entity-single",
					Status:   "Active",
					Message:  nil,
				},
			},
			expectError:   false,
			expectedCount: 1,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := sdd.CreateMultiple(ctx, nil, tc.inputs)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Verify results are returned in the same order as inputs
				for i, sd := range got {
					assert.NotEqual(t, uuid.Nil, sd.ID)
					assert.Equal(t, tc.inputs[i].EntityID, sd.EntityID, "result order should match input order")
					assert.Equal(t, tc.inputs[i].Status, sd.Status)
					assert.Equal(t, 1, sd.Count)
					assert.NotZero(t, sd.Created)
				}
			}
		})
	}
}

func TestStatusDetailSQLDAO_CreateMultiple_ExceedsMaxBatchItems(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()
	sdd := NewStatusDetailDAO(dbSession)

	// Create inputs exceeding MaxBatchItems
	inputs := make([]StatusDetailCreateInput, db.MaxBatchItems+1)
	for i := range inputs {
		inputs[i] = StatusDetailCreateInput{
			EntityID: uuid.NewString(),
			Status:   "Active",
		}
	}

	_, err := sdd.CreateMultiple(ctx, nil, inputs)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "batch size")
	assert.Contains(t, err.Error(), "exceeds maximum allowed")
}
