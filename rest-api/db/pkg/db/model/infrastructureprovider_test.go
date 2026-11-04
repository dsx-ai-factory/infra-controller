// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
)

func TestInfrastructureProviderSQLDAO_GetByID(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
		id  uuid.UUID
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)

	// Create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	if err != nil {
		t.Fatal(err)
	}

	ip := &InfrastructureProvider{
		ID:             uuid.New(),
		Name:           "test",
		DisplayName:    cutil.GetPtr("test"),
		Org:            "test-org",
		OrgDisplayName: cutil.GetPtr("Test Org"),
		CreatedBy:      uuid.New(),
	}

	_, err = dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// OTEL Spanner configuration
	ctx := context.Background()
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *InfrastructureProvider
		wantErr            bool
		wantErrVal         error
		verifyChildSpanner bool
	}{
		{
			name: "retrieve an InfrastructureProvider by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  ip.ID,
			},
			want:    ip,
			wantErr: false,
		},
		{
			name: "error retrieve an InfrastructureProvider by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				id:  uuid.New(),
			},
			want:       nil,
			wantErr:    true,
			wantErrVal: db.ErrDoesNotExist,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ipsd := InfrastructureProviderSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := ipsd.GetByID(tt.args.ctx, nil, tt.args.id, nil)
			if !tt.wantErr {
				require.NoError(t, err)
			} else {
				assert.Equal(t, tt.wantErrVal, err)
				return
			}

			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, *tt.want.DisplayName, *got.DisplayName)
			assert.Equal(t, tt.want.Org, got.Org)
			assert.Equal(t, *tt.want.OrgDisplayName, *got.OrgDisplayName)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfrastructureProviderSQLDAO_GetAllByOrg(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
		org string
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	if err != nil {
		t.Fatal(err)
	}

	org := "test-org"
	orgDisplayName := "Test Org"

	ip1 := InfrastructureProvider{
		ID:             uuid.New(),
		Name:           "test 1",
		DisplayName:    cutil.GetPtr("test 2"),
		Org:            org,
		OrgDisplayName: cutil.GetPtr(orgDisplayName),
		CreatedBy:      uuid.New(),
	}

	ip2 := InfrastructureProvider{
		ID:             uuid.New(),
		Name:           "test 2",
		DisplayName:    cutil.GetPtr("test 2"),
		Org:            org,
		OrgDisplayName: cutil.GetPtr(orgDisplayName),
		CreatedBy:      uuid.New(),
	}

	ips := []InfrastructureProvider{ip1, ip2}

	_, err = dbSession.DB.NewInsert().Model(&ips).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// OTEL Spanner configuration
	ctx := context.Background()
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               []InfrastructureProvider
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "retrieve all InfrastructureProvider by org ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				org: org,
			},
			want:               ips,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ipsd := InfrastructureProviderSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := ipsd.GetAllByOrg(tt.args.ctx, nil, tt.args.org, nil)
			if tt.wantErr {
				require.NotNil(t, err)
			} else {
				require.Nil(t, err)
				require.NotNil(t, got)
				assert.Equal(t, len(got), len(tt.want))
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfrastructureProviderSQLDAO_Create(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx   context.Context
		input InfrastructureProviderCreateInput
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	if err != nil {
		t.Fatal(err)
	}

	ip := &InfrastructureProvider{
		Name:           "test",
		DisplayName:    cutil.GetPtr("test"),
		Org:            "test-org",
		OrgDisplayName: cutil.GetPtr("Test Org"),
		CreatedBy:      uuid.New(),
	}

	// OTEL Spanner configuration
	ctx := context.Background()
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *InfrastructureProvider
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "create an InfrastructureProvider",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				input: InfrastructureProviderCreateInput{
					Name:           ip.Name,
					DisplayName:    ip.DisplayName,
					Org:            ip.Org,
					OrgDisplayName: ip.OrgDisplayName,
					CreatedBy:      ip.CreatedBy,
				},
			},
			want:               ip,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ipsd := InfrastructureProviderSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := ipsd.Create(tt.args.ctx, nil, tt.args.input)
			if tt.wantErr {
				require.NotNil(t, err)
			} else {
				require.Nil(t, err)
				require.NotNil(t, got)
			}

			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, *tt.want.DisplayName, *got.DisplayName)
			assert.Equal(t, tt.want.Org, got.Org)
			assert.Equal(t, *tt.want.OrgDisplayName, *got.OrgDisplayName)
			assert.Equal(t, tt.want.CreatedBy, got.CreatedBy)
			assert.NotEqual(t, tt.want.Updated.String(), got.Updated.String())

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfrastructureProviderSQLDAO_Update(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx   context.Context
		input InfrastructureProviderUpdateInput
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	if err != nil {
		t.Fatal(err)
	}

	// Create infrastructure provider
	ip := &InfrastructureProvider{
		ID:             uuid.New(),
		Name:           "test",
		DisplayName:    cutil.GetPtr("Test"),
		Org:            "test-org",
		OrgDisplayName: cutil.GetPtr("Test Org"),
		CreatedBy:      uuid.New(),
	}

	_, err = dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// Updated infrastructure provider
	uip := &InfrastructureProvider{
		ID:             ip.ID,
		Name:           "test2",
		DisplayName:    cutil.GetPtr("Test 2"),
		Org:            ip.Org,
		OrgDisplayName: cutil.GetPtr("Test Org Updated"),
		CreatedBy:      ip.CreatedBy,
	}

	// OTEL Spanner configuration
	ctx := context.Background()
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *InfrastructureProvider
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "update an InfrastructureProvider",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				input: InfrastructureProviderUpdateInput{
					InfrastructureProviderID: ip.ID,
					Name:                     cutil.GetPtr(uip.Name),
					DisplayName:              uip.DisplayName,
					OrgDisplayName:           uip.OrgDisplayName,
				},
			},
			want:               uip,
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ipsd := InfrastructureProviderSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := ipsd.Update(tt.args.ctx, nil, tt.args.input)
			if tt.wantErr {
				require.NotNil(t, err)
			} else {
				require.Nil(t, err)
				require.NotNil(t, got)
			}

			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, *tt.want.DisplayName, *got.DisplayName)
			assert.Equal(t, tt.want.Org, got.Org)
			assert.NotEqual(t, tt.want.Updated.String(), got.Updated.String())

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInfrastructureProviderSQLDAO_Delete(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
		id  uuid.UUID
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	if err != nil {
		t.Fatal(err)
	}

	ip := &InfrastructureProvider{
		ID:             uuid.New(),
		Name:           "test",
		DisplayName:    cutil.GetPtr("test"),
		Org:            "test-org",
		OrgDisplayName: cutil.GetPtr("Test Org"),
	}

	_, err = dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// OTEL Spanner configuration
	ctx := context.Background()
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "delete InfrastructureProvider by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  ip.ID,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ipsd := InfrastructureProviderSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			derr := ipsd.Delete(tt.args.ctx, nil, tt.args.id)
			if tt.wantErr {
				require.NotNil(t, derr)
			} else {
				require.Nil(t, derr)
			}

			dip := &InfrastructureProvider{}
			err := dbSession.DB.NewSelect().Model(dip).WhereDeleted().Where("id = ?", ip.ID).Scan(context.Background())
			if err != nil {
				t.Fatal(err)
			}

			assert.NotNil(t, dip.Deleted)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
