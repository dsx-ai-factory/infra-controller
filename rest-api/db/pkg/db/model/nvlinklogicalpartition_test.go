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
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
)

func TestNVLinkLogicalPartition_ToProto(t *testing.T) {
	id := uuid.New()
	t.Run("includes description when set", func(t *testing.T) {
		desc := "primary"
		nvllp := &NVLinkLogicalPartition{ID: id, Name: "nvllp-a", Org: "org-1", Description: &desc}
		got := nvllp.ToProto()
		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, id.String(), got.Id.Value)
		require.NotNil(t, got.Config)
		assert.Equal(t, "org-1", got.Config.TenantOrganizationId)
		require.NotNil(t, got.Config.Metadata)
		assert.Equal(t, "nvllp-a", got.Config.Metadata.Name)
		assert.Equal(t, "primary", got.Config.Metadata.Description)
	})

	t.Run("populates metadata.name even when description is nil", func(t *testing.T) {
		nvllp := &NVLinkLogicalPartition{ID: id, Name: "nvllp-a", Org: "org-1"}
		got := nvllp.ToProto()
		require.NotNil(t, got)
		require.NotNil(t, got.Config)
		require.NotNil(t, got.Config.Metadata)
		assert.Equal(t, "nvllp-a", got.Config.Metadata.Name)
		assert.Equal(t, "", got.Config.Metadata.Description)
	})
}

func TestNVLinkLogicalPartition_FromProto(t *testing.T) {
	t.Run("nil proto is a no-op", func(t *testing.T) {
		id := uuid.New()
		desc := "kept"
		nvllp := &NVLinkLogicalPartition{ID: id, Name: "kept", Org: "org-1", Description: &desc}
		nvllp.FromProto(nil)
		assert.Equal(t, id, nvllp.ID)
		assert.Equal(t, "kept", nvllp.Name)
		assert.Equal(t, "org-1", nvllp.Org)
		require.NotNil(t, nvllp.Description)
		assert.Equal(t, "kept", *nvllp.Description)
	})

	t.Run("populates from proto metadata", func(t *testing.T) {
		id := uuid.New()
		nvllp := &NVLinkLogicalPartition{ID: uuid.New()}
		nvllp.FromProto(&corev1.NVLinkLogicalPartition{
			Id: &corev1.NVLinkLogicalPartitionId{Value: id.String()},
			Config: &corev1.NVLinkLogicalPartitionConfig{
				TenantOrganizationId: "org-1",
				Metadata:             &corev1.Metadata{Name: "nvllp-a", Description: "primary"},
			},
		})
		assert.Equal(t, id, nvllp.ID)
		assert.Equal(t, "nvllp-a", nvllp.Name)
		assert.Equal(t, "org-1", nvllp.Org)
		require.NotNil(t, nvllp.Description)
		assert.Equal(t, "primary", *nvllp.Description)
	})

	t.Run("clears Description when proto omits it", func(t *testing.T) {
		desc := "existing"
		nvllp := &NVLinkLogicalPartition{ID: uuid.New(), Name: "n", Description: &desc}
		nvllp.FromProto(&corev1.NVLinkLogicalPartition{
			Config: &corev1.NVLinkLogicalPartitionConfig{
				Metadata: &corev1.Metadata{Name: "n"},
			},
		})
		assert.Nil(t, nvllp.Description)
	})

	t.Run("preserves ID when proto Id is unparseable", func(t *testing.T) {
		id := uuid.New()
		nvllp := &NVLinkLogicalPartition{ID: id}
		nvllp.FromProto(&corev1.NVLinkLogicalPartition{
			Id: &corev1.NVLinkLogicalPartitionId{Value: "not-a-uuid"},
		})
		assert.Equal(t, id, nvllp.ID)
	})
}

func TestNVLinkLogicalPartition_ToDeletionRequestProto(t *testing.T) {
	id := uuid.New()
	nvllp := &NVLinkLogicalPartition{ID: id}
	req := nvllp.ToDeletionRequestProto()
	require.NotNil(t, req)
	require.NotNil(t, req.Id)
	assert.Equal(t, id.String(), req.Id.Value)
}

func TestNVLinkLogicalPartition_Validate(t *testing.T) {
	valid := &NVLinkLogicalPartition{
		Name:   "test-nvllp",
		Status: NVLinkLogicalPartitionStatusReady,
	}

	t.Run("populated partition is valid", func(t *testing.T) {
		assert.NoError(t, valid.Validate())
	})
	t.Run("empty Status errors", func(t *testing.T) {
		nvllp := *valid
		nvllp.Status = ""
		assert.Error(t, nvllp.Validate())
	})
	t.Run("invalid Status errors", func(t *testing.T) {
		nvllp := *valid
		nvllp.Status = "Bogus"
		assert.Error(t, nvllp.Validate())
	})
	t.Run("empty Name errors", func(t *testing.T) {
		nvllp := *valid
		nvllp.Name = ""
		assert.Error(t, nvllp.Validate())
	})
	t.Run("Name with leading whitespace errors", func(t *testing.T) {
		nvllp := *valid
		nvllp.Name = " test-nvllp"
		assert.Error(t, nvllp.Validate())
	})
	t.Run("Name with trailing whitespace errors", func(t *testing.T) {
		nvllp := *valid
		nvllp.Name = "test-nvllp "
		assert.Error(t, nvllp.Validate())
	})
	t.Run("single-character Name errors (too short)", func(t *testing.T) {
		nvllp := *valid
		nvllp.Name = "x"
		assert.Error(t, nvllp.Validate())
	})
}

func testNVLinkLogicalPartitionSetupSchema(t *testing.T, dbSession *db.Session) {
	// Create tables
	err := dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*NVLinkLogicalPartition)(nil))
	require.NoError(t, err)
}

func TestNVLinkLogicalPartitionSQLDAO_GetByID(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
		id  uuid.UUID
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testNVLinkLogicalPartitionSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-NVLinkLogicalPartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *NVLinkLogicalPartition
		wantErr            error
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			name: "get NVLinkLogicalPartition by ID returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  nvllp.ID,
			},
			want:               nvllp,
			wantErr:            nil,
			paramRelations:     []string{TenantRelationName, SiteRelationName},
			verifyChildSpanner: true,
		},
		{
			name: "get NVLinkLogicalPartition by non-existent ID returns error",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
				id:  uuid.New(),
			},
			want:    nil,
			wantErr: db.ErrDoesNotExist,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvllpsd := NVLinkLogicalPartitionSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}

			got, err := nvllpsd.GetByID(tt.args.ctx, nil, tt.args.id, tt.paramRelations)
			if tt.wantErr != nil {
				assert.ErrorAs(t, err, &tt.wantErr)
				return
			}
			if err == nil {
				if len(tt.paramRelations) > 0 {
					assert.NotNil(t, got.Site)
					assert.NotNil(t, got.Tenant)
				}
				assert.EqualValues(t, tt.want.ID, got.ID)
			}
			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkLogicalPartition_GetAll(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}

	type args struct {
		ctx            context.Context
		names          []string
		ids            []uuid.UUID
		tenantIDs      []uuid.UUID
		siteIDs        []uuid.UUID
		orgs           []string
		searchQuery    *string
		statuses       []string
		offset         *int
		limit          *int
		orderBy        *paginator.OrderBy
		paramRelations []string
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testNVLinkLogicalPartitionSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))

	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu1 := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("janed@test.com"), cutil.GetPtr("Jane"), cutil.GetPtr("Doe"))
	tn1 := testBuildTenant(t, dbSession, nil, "test-tenant-1", "test-tenant-org-1", tnu1.ID)

	tnu2 := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jimd@test.com"), cutil.GetPtr("Jim"), cutil.GetPtr("Doe"))
	tn2 := testBuildTenant(t, dbSession, nil, "test-tenant-2", "test-tenant-org-2", tnu2.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	totalCount := 30

	nvlinkLogicalPartitions := []NVLinkLogicalPartition{}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	for i := 0; i < totalCount; i++ {
		var nvllp *NVLinkLogicalPartition
		var tn *Tenant

		if i%2 == 0 {
			tn = tn1
		} else {
			tn = tn2
		}

		if i%2 == 0 {
			nvllp = testBuildNVLinkLogicalPartition(t, dbSession, nil, fmt.Sprintf("test-NVLinkLogicalPartition-batch-v1-%v", i), cutil.GetPtr(fmt.Sprintf("test-NVLinkLogicalPartition-desc-batch-1-%v", i)), tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tn.CreatedBy)
		} else {
			nvllp = testBuildNVLinkLogicalPartition(t, dbSession, nil, fmt.Sprintf("test-NVLinkLogicalPartition-batch-v2-%v", i), cutil.GetPtr(fmt.Sprintf("test-NVLinkLogicalPartition-desc-batch-2-%v", i)), tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusDeleting), tn.CreatedBy)
		}

		nvlinkLogicalPartitions = append(nvlinkLogicalPartitions, *nvllp)
	}

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantCount          int
		wantTotalCount     int
		wantFirstEntry     *NVLinkLogicalPartition
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "get all NVLinkLogicalPartitions with no filters returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       ctx,
				tenantIDs: nil,
				siteIDs:   nil,
				orgs:      nil,
			},
			wantCount:          paginator.DefaultLimit,
			wantTotalCount:     totalCount,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "get all NVLinkLogicalPartitions with relation returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:            context.Background(),
				tenantIDs:      nil,
				siteIDs:        nil,
				orgs:           nil,
				paramRelations: []string{TenantRelationName, SiteRelationName},
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with Tenant ID filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				tenantIDs: []uuid.UUID{tn1.ID},
				siteIDs:   nil,
				orgs:      nil,
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with Tenant ID and name filters returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				names:     []string{"test-NVLinkLogicalPartition-batch-v1-8"},
				tenantIDs: []uuid.UUID{tn1.ID},
				siteIDs:   nil,
				orgs:      nil,
			},
			wantCount:      1,
			wantTotalCount: 1,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with Site ID filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				tenantIDs: nil,
				siteIDs:   []uuid.UUID{st.ID},
				orgs:      nil,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with Org filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				tenantIDs: nil,
				siteIDs:   nil,
				orgs:      []string{tn1.Org},
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all with limit returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				tenantIDs: nil,
				siteIDs:   []uuid.UUID{st.ID},
				orgs:      nil,
				limit:     cutil.GetPtr(10),
			},
			wantCount:      10,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all with offset returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				tenantIDs: []uuid.UUID{tn1.ID},
				siteIDs:   nil,
				orgs:      nil,
				offset:    cutil.GetPtr(5),
			},
			wantCount:      10,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all ordered by name",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				tenantIDs: []uuid.UUID{tn1.ID},
				siteIDs:   nil,
				orgs:      nil,
				orderBy:   &paginator.OrderBy{Field: "name", Order: paginator.OrderDescending},
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantFirstEntry: &nvlinkLogicalPartitions[8],
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with Org filter with site/tenant include relation returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:            context.Background(),
				tenantIDs:      nil,
				siteIDs:        nil,
				orgs:           []string{tn1.Org},
				paramRelations: []string{SiteRelationName, TenantRelationName},
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with infrastructure ID filter returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				tenantIDs: nil,
				siteIDs:   nil,
				orgs:      nil,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: totalCount,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with search query as name returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantIDs:   nil,
				siteIDs:     nil,
				orgs:        nil,
				searchQuery: cutil.GetPtr("test-NVLinkLogicalPartition-batch-v1-"),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with search query as a description returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantIDs:   nil,
				siteIDs:     nil,
				orgs:        nil,
				searchQuery: cutil.GetPtr("test-NVLinkLogicalPartition-desc-batch-1-"),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with search query as a status ready returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				ids:         nil,
				tenantIDs:   nil,
				siteIDs:     nil,
				orgs:        nil,
				searchQuery: db.GetTypedStrPtr(NVLinkLogicalPartitionStatusReady),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with search query as a status deleting returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantIDs:   nil,
				siteIDs:     nil,
				orgs:        nil,
				searchQuery: db.GetTypedStrPtr(NVLinkLogicalPartitionStatusDeleting),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with search query with combination of name and status returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantIDs:   nil,
				siteIDs:     nil,
				orgs:        nil,
				searchQuery: cutil.GetPtr("test-NVLinkLogicalPartition-batch-v1- | ready"),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with search query with combination of description and status returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantIDs:   nil,
				siteIDs:     nil,
				orgs:        nil,
				searchQuery: cutil.GetPtr("test-NVLinkLogicalPartition-desc-batch-1- error"),
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with search query with combination of description and status returns none success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantIDs:   nil,
				siteIDs:     nil,
				orgs:        nil,
				searchQuery: cutil.GetPtr("test-NVLinkLogicalPartition-desc-batch-3- error"),
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with empty search query returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantIDs:   nil,
				siteIDs:     nil,
				orgs:        nil,
				searchQuery: cutil.GetPtr(""),
			},
			wantCount:      20,
			wantTotalCount: 30,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with empty search query returns success with ip",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         context.Background(),
				tenantIDs:   nil,
				siteIDs:     nil,
				orgs:        nil,
				searchQuery: cutil.GetPtr(""),
			},
			wantCount:      20,
			wantTotalCount: 30,
			wantErr:        false,
		},
		{
			name: "get all NVLinkLogicalPartitions with status returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:       context.Background(),
				tenantIDs: nil,
				siteIDs:   nil,
				orgs:      nil,
				statuses:  []string{string(NVLinkLogicalPartitionStatusDeleting)},
			},
			wantCount:      totalCount / 2,
			wantTotalCount: totalCount / 2,
			wantErr:        false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvllpsd := NVLinkLogicalPartitionSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}

			got, total, err := nvllpsd.GetAll(
				tt.args.ctx,
				nil,
				NVLinkLogicalPartitionFilterInput{
					Names:                     tt.args.names,
					SiteIDs:                   tt.args.siteIDs,
					TenantOrgs:                tt.args.orgs,
					TenantIDs:                 tt.args.tenantIDs,
					Statuses:                  tt.args.statuses,
					NVLinkLogicalPartitionIDs: tt.args.ids,
					SearchQuery:               tt.args.searchQuery,
				},
				paginator.PageInput{
					Offset:  tt.args.offset,
					Limit:   tt.args.limit,
					OrderBy: tt.args.orderBy,
				},
				tt.args.paramRelations,
			)
			if tt.wantErr {
				require.Error(t, err)
			}

			assert.Equal(t, tt.wantCount, len(got))
			assert.Equal(t, tt.wantTotalCount, total)

			if len(got) > 0 && len(tt.args.paramRelations) > 0 {
				assert.NotNil(t, got[0].Site)
				assert.NotNil(t, got[0].Tenant)
			}

			if tt.wantFirstEntry != nil {
				assert.Equal(t, tt.wantFirstEntry.Name, got[0].Name)
			}

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkLogicalPartitionSQLDAO_Create(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx         context.Context
		name        string
		description *string
		org         string
		siteID      uuid.UUID
		tenantID    uuid.UUID
		status      NVLinkLogicalPartitionStatus
		createdBy   User
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testNVLinkLogicalPartitionSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	nvllp := &NVLinkLogicalPartition{
		Name:        "test-NVLinkLogicalPartition",
		Description: cutil.GetPtr("Test NVLinkLogicalPartition"),
		Org:         tn.Org,
		SiteID:      st.ID,
		TenantID:    tn.ID,
		Status:      NVLinkLogicalPartitionStatusPending,
		CreatedBy:   tnu.ID,
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *NVLinkLogicalPartition
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "create NVLinkLogicalPartition from params returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         ctx,
				name:        nvllp.Name,
				description: nvllp.Description,
				org:         nvllp.Org,
				tenantID:    nvllp.TenantID,
				siteID:      nvllp.SiteID,
				status:      nvllp.Status,
				createdBy:   User{ID: nvllp.CreatedBy},
			},
			want:               nvllp,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "create with invalid status returns error",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:         ctx,
				name:        "invalid-status-nvllp",
				description: nvllp.Description,
				org:         nvllp.Org,
				tenantID:    nvllp.TenantID,
				siteID:      nvllp.SiteID,
				status:      NVLinkLogicalPartitionStatus("Bogus"),
				createdBy:   User{ID: nvllp.CreatedBy},
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvllpsd := NVLinkLogicalPartitionSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}
			got, err := nvllpsd.Create(
				tt.args.ctx,
				nil,
				NVLinkLogicalPartitionCreateInput{
					Name:        tt.args.name,
					Description: tt.args.description,
					TenantOrg:   tt.args.org,
					SiteID:      tt.args.siteID,
					TenantID:    tt.args.tenantID,
					Status:      tt.args.status,
					CreatedBy:   tt.args.createdBy.ID,
				},
			)
			require.Equal(t, tt.wantErr, err != nil)
			if tt.wantErr {
				return
			}

			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, *tt.want.Description, *got.Description)
			assert.Equal(t, tt.want.Org, got.Org)
			assert.Equal(t, tt.want.SiteID, got.SiteID)
			assert.Equal(t, tt.want.TenantID, got.TenantID)
			assert.Equal(t, tt.want.Status, got.Status)
			assert.Equal(t, tt.want.CreatedBy, got.CreatedBy)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkLogicalPartitionSQLDAO_Update(t *testing.T) {
	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testNVLinkLogicalPartitionSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "test-provider-org", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-NVLinkLogicalPartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)

	uNVLinkLogicalPartition := &NVLinkLogicalPartition{
		Name:            "test-updated",
		Description:     cutil.GetPtr("Test Updated"),
		Status:          NVLinkLogicalPartitionStatusReady,
		IsMissingOnSite: true,
	}

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx             context.Context
		id              uuid.UUID
		name            *string
		description     *string
		Status          NVLinkLogicalPartitionStatus
		IsMissingOnSite bool
	}
	tests := []struct {
		name               string
		fields             fields
		args               args
		want               *NVLinkLogicalPartition
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "update NVLinkLogicalPartition from params returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:             ctx,
				id:              nvllp.ID,
				name:            &uNVLinkLogicalPartition.Name,
				description:     uNVLinkLogicalPartition.Description,
				Status:          uNVLinkLogicalPartition.Status,
				IsMissingOnSite: uNVLinkLogicalPartition.IsMissingOnSite,
			},
			want:               uNVLinkLogicalPartition,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "update with invalid status returns error",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:             ctx,
				id:              nvllp.ID,
				name:            &uNVLinkLogicalPartition.Name,
				description:     uNVLinkLogicalPartition.Description,
				Status:          NVLinkLogicalPartitionStatus("Bogus"),
				IsMissingOnSite: uNVLinkLogicalPartition.IsMissingOnSite,
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvllpsd := NVLinkLogicalPartitionSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}
			got, err := nvllpsd.Update(
				tt.args.ctx,
				nil,
				NVLinkLogicalPartitionUpdateInput{
					NVLinkLogicalPartitionID: tt.args.id,
					Name:                     tt.args.name,
					Description:              tt.args.description,
					Status:                   &tt.args.Status,
					IsMissingOnSite:          &tt.args.IsMissingOnSite,
				},
			)

			require.Equal(t, tt.wantErr, err != nil)
			if tt.wantErr {
				return
			}

			fmt.Printf("\ngot ID: %v, Created: %v, Updated: %v", got.ID.String(), got.Created, got.Updated)

			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, *tt.want.Description, *got.Description)
			assert.Equal(t, tt.want.Status, got.Status)
			assert.NotEqualValues(t, got.Updated, nvllp.Updated)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkLogicalPartitionSQLDAO_Delete(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
		id  uuid.UUID
	}

	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testNVLinkLogicalPartitionSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-NVLinkLogicalPartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "delete NVLinkLogicalPartition by ID",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: ctx,
				id:  nvllp.ID,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvllpsd := NVLinkLogicalPartitionSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: stracer.NewTracerSpan(),
			}

			err := nvllpsd.Delete(tt.args.ctx, nil, tt.args.id)
			require.Equal(t, tt.wantErr, err != nil)

			dNVLinkLogicalPartition := &NVLinkLogicalPartition{}
			err = dbSession.DB.NewSelect().Model(dNVLinkLogicalPartition).WhereDeleted().Where("id = ?", nvllp.ID).Scan(context.Background())
			require.NoError(t, err)
			assert.NotNil(t, dNVLinkLogicalPartition.Deleted)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNVLinkLogicalPartitionSQLDAO_Clear(t *testing.T) {
	// Create test DB
	dbSession := testInitDB(t)
	defer dbSession.Close()

	// Create tables
	testNVLinkLogicalPartitionSetupSchema(t, dbSession)

	// Create necessary objects
	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "test-provider-org", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	nvllp := testBuildNVLinkLogicalPartition(t, dbSession, nil, "test-NVLinkLogicalPartition", nil, tn.Org, tn.ID, st.ID, cutil.GetPtr(NVLinkLogicalPartitionStatusReady), tnu.ID)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	type fields struct {
		dbSession  *db.Session
		tracerSpan *stracer.TracerSpan
	}
	type args struct {
		ctx                                context.Context
		tx                                 *db.Tx
		id                                 uuid.UUID
		description                        bool
		ControllerNVLinkLogicalPartitionID bool
	}
	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "clearing NVLinkLogicalPartition attributes returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                                ctx,
				id:                                 nvllp.ID,
				description:                        true,
				ControllerNVLinkLogicalPartitionID: true,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvllpsd := NVLinkLogicalPartitionSQLDAO{
				dbSession:  tt.fields.dbSession,
				tracerSpan: tt.fields.tracerSpan,
			}
			got, err := nvllpsd.Clear(
				tt.args.ctx,
				tt.args.tx,
				NVLinkLogicalPartitionClearInput{
					NVLinkLogicalPartitionID: tt.args.id,
					Description:              tt.args.description,
				},
			)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}

			if tt.args.description {
				assert.Nil(t, got.Description)
			}

		})
	}
}
