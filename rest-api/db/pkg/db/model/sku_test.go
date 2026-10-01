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
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
	"google.golang.org/protobuf/types/known/timestamppb"
)

func TestSkuComponents_Equal(t *testing.T) {
	t.Run("nil wrapper equals nil wrapper", func(t *testing.T) {
		var a, b *SkuComponents
		assert.True(t, a.Equal(b))
	})
	t.Run("nil wrapper equals wrapper with nil inner", func(t *testing.T) {
		var a *SkuComponents
		b := &SkuComponents{}
		assert.True(t, a.Equal(b))
		assert.True(t, b.Equal(a))
	})
	t.Run("two wrappers with nil inner are equal", func(t *testing.T) {
		assert.True(t, (&SkuComponents{}).Equal(&SkuComponents{}))
	})
	t.Run("identical inner protos are equal", func(t *testing.T) {
		a := &SkuComponents{SkuComponents: &corev1.SkuComponents{}}
		b := &SkuComponents{SkuComponents: &corev1.SkuComponents{}}
		assert.True(t, a.Equal(b))
	})
	t.Run("nil inner does not equal non-nil inner", func(t *testing.T) {
		a := &SkuComponents{}
		b := &SkuComponents{SkuComponents: &corev1.SkuComponents{}}
		assert.False(t, a.Equal(b))
		assert.False(t, b.Equal(a))
	})
}

func TestSKU_ToProto(t *testing.T) {
	siteID := uuid.New()
	deviceType := "GPU"
	created := time.Date(2025, time.January, 2, 3, 4, 5, 678_901_000, time.UTC)

	t.Run("populates proto from receiver", func(t *testing.T) {
		sk := &SKU{
			ID:                   "sku-1",
			SiteID:               siteID,
			Description:          "GPU compute SKU",
			SchemaVersion:        5,
			DeviceType:           &deviceType,
			Created:              created,
			Components:           &SkuComponents{SkuComponents: &corev1.SkuComponents{}},
			AssociatedMachineIds: []string{"m-1", "m-2"},
		}
		proto := sk.ToProto()
		require.NotNil(t, proto)
		assert.Equal(t, "sku-1", proto.Id)
		assert.Equal(t, "GPU compute SKU", proto.GetDescription())
		assert.Equal(t, uint32(5), proto.SchemaVersion)
		assert.Equal(t, &deviceType, proto.DeviceType)
		require.NotNil(t, proto.Created)
		assert.Equal(t, created, proto.Created.AsTime())
		require.NotNil(t, proto.Components)
		require.Len(t, proto.AssociatedMachineIds, 2)
		assert.Equal(t, "m-1", proto.AssociatedMachineIds[0].Id)
		assert.Equal(t, "m-2", proto.AssociatedMachineIds[1].Id)
	})

	t.Run("nil Components yields nil proto.Components", func(t *testing.T) {
		sk := &SKU{ID: "sku-2"}
		proto := sk.ToProto()
		require.NotNil(t, proto)
		assert.Nil(t, proto.Created)
		assert.Nil(t, proto.Components)
	})

	t.Run("nil AssociatedMachineIds yields nil proto.AssociatedMachineIds", func(t *testing.T) {
		sk := &SKU{ID: "sku-3"}
		proto := sk.ToProto()
		require.NotNil(t, proto)
		assert.Nil(t, proto.AssociatedMachineIds)
	})
}

func TestSKU_FromProto(t *testing.T) {
	siteID := uuid.New()
	deviceType := "GPU"
	created := time.Date(2025, time.January, 2, 3, 4, 5, 678_901_234, time.UTC)
	normalizedCreated := created.Round(time.Microsecond)

	t.Run("nil proto leaves receiver unchanged", func(t *testing.T) {
		sk := &SKU{ID: "preserved", SiteID: siteID}
		sk.FromProto(nil, uuid.New())
		assert.Equal(t, "preserved", sk.ID)
		assert.Equal(t, siteID, sk.SiteID)
	})

	t.Run("populates fields from proto", func(t *testing.T) {
		sk := &SKU{}
		sk.FromProto(&corev1.Sku{
			Id:            "sku-1",
			Description:   cutil.GetPtr("GPU compute SKU"),
			SchemaVersion: 4,
			DeviceType:    &deviceType,
			Created:       timestamppb.New(created),
			Components:    &corev1.SkuComponents{},
			AssociatedMachineIds: []*corev1.MachineId{
				{Id: "m-1"},
				{Id: ""}, // skipped
				{Id: "m-2"},
			},
		}, siteID)
		assert.Equal(t, "sku-1", sk.ID)
		assert.Equal(t, siteID, sk.SiteID)
		assert.Equal(t, "GPU compute SKU", sk.Description)
		assert.Equal(t, uint32(4), sk.SchemaVersion)
		assert.Equal(t, &deviceType, sk.DeviceType)
		assert.Equal(t, normalizedCreated, sk.Created)
		assert.Equal(t, []string{"m-1", "m-2"}, sk.AssociatedMachineIds)
		require.NotNil(t, sk.Components)
	})

	t.Run("missing created timestamp clears stale value", func(t *testing.T) {
		sk := &SKU{Created: created}
		sk.FromProto(&corev1.Sku{Id: "sku-1"}, siteID)
		assert.True(t, sk.Created.IsZero())
	})

	t.Run("invalid created timestamp is treated as missing", func(t *testing.T) {
		sk := &SKU{}
		sk.FromProto(&corev1.Sku{
			Id:      "sku-1",
			Created: &timestamppb.Timestamp{Seconds: 253_402_300_800},
		}, siteID)
		assert.True(t, sk.Created.IsZero())
	})

	t.Run("nil description clears existing description", func(t *testing.T) {
		sk := &SKU{Description: "stale description"}
		sk.FromProto(&corev1.Sku{Id: "sku-1"}, siteID)
		assert.Empty(t, sk.Description)
	})

	t.Run("nil Components yields nil wrapper", func(t *testing.T) {
		sk := &SKU{Components: &SkuComponents{SkuComponents: &corev1.SkuComponents{}}}
		sk.FromProto(&corev1.Sku{Id: "sku-1"}, siteID)
		assert.Nil(t, sk.Components)
	})

	t.Run("nil AssociatedMachineIds yields nil slice", func(t *testing.T) {
		sk := &SKU{}
		sk.FromProto(&corev1.Sku{Id: "sku-1"}, siteID)
		assert.Nil(t, sk.AssociatedMachineIds)
	})
}

// reset the tables needed for SKU tests
func testSKUSetupSchema(t *testing.T, dbSession *db.Session) {
	// create User table
	err := dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create InfrastructureProvider table
	err = dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	assert.Nil(t, err)
	// create the SKU table
	err = dbSession.DB.ResetModel(context.Background(), (*SKU)(nil))
	assert.Nil(t, err)
}

func testSKUInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	return dbSession
}

func testSkuCreateSkus(ctx context.Context, t *testing.T, dbSession *db.Session, siteId uuid.UUID) (created []SKU) {
	ssd := NewSkuDAO(dbSession)

	ids := []string{"sku-1", "sku-2", "sku-3"}
	for _, id := range ids {
		protoSku := &corev1.SkuComponents{}
		sk, err := ssd.Create(ctx, nil, SkuCreateInput{
			SkuID:       id,
			SiteID:      siteId,
			Description: id + " description",
			Components:  &SkuComponents{SkuComponents: protoSku},
		})
		require.NoError(t, err)
		require.NotNil(t, sk)
		created = append(created, *sk)
	}
	return
}

func TestSkuSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testSKUInitDB(t)
	defer dbSession.Close()
	testSKUSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	ssd := NewSkuDAO(dbSession)
	authoritativeCreated := time.Date(2025, time.January, 2, 3, 4, 5, 678_901_234, time.UTC)
	normalizedAuthoritativeCreated := authoritativeCreated.Round(time.Microsecond)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []SkuCreateInput
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:               "create one",
			inputs:             []SkuCreateInput{{SkuID: "sku-1", SiteID: site.ID, Created: &authoritativeCreated, Description: "first SKU", SchemaVersion: 5, Components: &SkuComponents{SkuComponents: &corev1.SkuComponents{}}}},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:        "create multiple",
			inputs:      []SkuCreateInput{{SkuID: "sku-2", SiteID: site.ID, Description: "second SKU", Components: &SkuComponents{SkuComponents: &corev1.SkuComponents{}}}, {SkuID: "sku-3", SiteID: site.ID, Description: "third SKU", Components: &SkuComponents{SkuComponents: &corev1.SkuComponents{}}}},
			expectError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, input := range tc.inputs {
				beforeCreate := time.Now().UTC()
				got, err := ssd.Create(ctx, nil, input)
				afterCreate := time.Now().UTC()
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, got)
					assert.Equal(t, input.SkuID, got.ID)
					assert.Equal(t, input.SchemaVersion, got.SchemaVersion)
					assert.Equal(t, input.Description, got.Description)
					if input.Components != nil {
						assert.NotNil(t, got.Components)
					}
					if input.Created != nil {
						assert.True(t, normalizedAuthoritativeCreated.Equal(got.Created))
					} else {
						assert.False(t, got.Created.Before(beforeCreate))
						assert.False(t, got.Created.After(afterCreate))
					}
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSkuSQLDAO_Get(t *testing.T) {
	ctx := context.Background()
	dbSession := testSKUInitDB(t)
	defer dbSession.Close()
	testSKUSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	created := testSkuCreateSkus(ctx, t, dbSession, site.ID)
	ssd := NewSkuDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 string
		expectError        bool
		verifyChildSpanner bool
	}{
		{desc: "success existing", id: created[0].ID, verifyChildSpanner: true},
		{desc: "not found", id: "does-not-exist", expectError: true},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := ssd.Get(ctx, nil, tc.id)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.id, got.ID)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestSkuSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testSKUInitDB(t)
	defer dbSession.Close()
	testSKUSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	created := testSkuCreateSkus(ctx, t, dbSession, site.ID)
	ssd := NewSkuDAO(dbSession)

	// Populate associated machine IDs for filter testing
	_, err := ssd.Update(ctx, nil, SkuUpdateInput{SkuID: created[0].ID, AssociatedMachineIds: []string{"machine-1", "machine-2"}})
	require.NoError(t, err)
	_, err = ssd.Update(ctx, nil, SkuUpdateInput{SkuID: created[2].ID, AssociatedMachineIds: []string{"machine-2"}})
	require.NoError(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc          string
		filter        SkuFilterInput
		pageInput     paginator.PageInput
		expectedCount int
		expectedTotal *int
	}{
		{desc: "no filters", expectedCount: 3, expectedTotal: cutil.GetPtr(3)},
		{desc: "filter IDs", filter: SkuFilterInput{SkuIDs: []string{created[0].ID, created[2].ID}}, expectedCount: 2},
		{desc: "limit applies", pageInput: paginator.PageInput{Offset: cutil.GetPtr(0), Limit: cutil.GetPtr(2)}, expectedCount: 2, expectedTotal: cutil.GetPtr(3)},
		{desc: "offset applies", pageInput: paginator.PageInput{Offset: cutil.GetPtr(1)}, expectedCount: 2, expectedTotal: cutil.GetPtr(3)},
		{desc: "filter associated machine IDs", filter: SkuFilterInput{AssociatedMachineIds: []string{"machine-2"}}, expectedCount: 2},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := ssd.GetAll(ctx, nil, tc.filter, tc.pageInput)
			require.NoError(t, err)
			assert.Equal(t, tc.expectedCount, len(got))
			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}
			// tracer
			span := otrace.SpanFromContext(ctx)
			assert.True(t, span.SpanContext().IsValid())
		})
	}
}

func TestSkuSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testSKUInitDB(t)
	defer dbSession.Close()
	testSKUSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	created := testSkuCreateSkus(ctx, t, dbSession, site.ID)
	ssd := NewSkuDAO(dbSession)
	authoritativeCreated := time.Date(2025, time.January, 2, 3, 4, 5, 678_901_234, time.UTC)
	normalizedAuthoritativeCreated := authoritativeCreated.Round(time.Microsecond)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                string
		input               SkuUpdateInput
		expectedDescription string
		expectedVersion     uint32
		expectedCreated     time.Time
	}{
		{
			desc: "update sku data",
			input: SkuUpdateInput{
				SkuID:         created[0].ID,
				Created:       &authoritativeCreated,
				Description:   cutil.GetPtr("updated description"),
				SchemaVersion: cutil.GetPtr(uint32(5)),
				Components:    &SkuComponents{SkuComponents: &corev1.SkuComponents{}},
			},
			expectedDescription: "updated description",
			expectedVersion:     5,
			expectedCreated:     normalizedAuthoritativeCreated,
		},
		{
			desc: "nil description preserves stored description",
			input: SkuUpdateInput{
				SkuID:       created[1].ID,
				Description: nil,
				Components:  &SkuComponents{SkuComponents: &corev1.SkuComponents{}},
			},
			expectedDescription: created[1].Description,
			expectedVersion:     created[1].SchemaVersion,
			expectedCreated:     created[1].Created,
		},
		{
			desc: "empty description clears stored description",
			input: SkuUpdateInput{
				SkuID:       created[2].ID,
				Description: cutil.GetPtr(""),
			},
			expectedDescription: "",
			expectedVersion:     created[2].SchemaVersion,
			expectedCreated:     created[2].Created,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := ssd.Update(ctx, nil, tc.input)
			require.NoError(t, err)
			require.NotNil(t, got)
			assert.Equal(t, tc.input.SkuID, got.ID)
			assert.Equal(t, tc.expectedDescription, got.Description)
			assert.Equal(t, tc.expectedVersion, got.SchemaVersion)
			assert.True(t, tc.expectedCreated.Equal(got.Created))
			if tc.input.Components != nil {
				assert.NotNil(t, got.Components)
			}
			// tracer
			span := otrace.SpanFromContext(ctx)
			assert.True(t, span.SpanContext().IsValid())
		})
	}
}

func TestSkuSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testSKUInitDB(t)
	defer dbSession.Close()
	testSKUSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	created := testSkuCreateSkus(ctx, t, dbSession, site.ID)
	ssd := NewSkuDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc      string
		id        string
		wantErr   bool
		checkGone bool
	}{
		{desc: "delete existing", id: created[1].ID, checkGone: true},
		{desc: "delete non-existing ok", id: "not-exist", wantErr: false},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := ssd.Delete(ctx, nil, tc.id)
			assert.Equal(t, tc.wantErr, err != nil)
			if tc.checkGone && !tc.wantErr {
				_, err := ssd.Get(ctx, nil, tc.id)
				assert.NotNil(t, err)
			}
			// tracer
			span := otrace.SpanFromContext(ctx)
			assert.True(t, span.SpanContext().IsValid())
		})
	}
}

func TestSkuSQLDAO_Create_DefaultAssociatedMachineIds(t *testing.T) {
	ctx := context.Background()
	dbSession := testSKUInitDB(t)
	defer dbSession.Close()
	testSKUSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	ssd := NewSkuDAO(dbSession)

	// Create a SKU without specifying AssociatedMachineIds
	protoSku := &corev1.SkuComponents{}
	created, err := ssd.Create(ctx, nil, SkuCreateInput{
		SkuID:      "sku-default-test",
		Components: &SkuComponents{SkuComponents: protoSku},
		SiteID:     site.ID,
		// AssociatedMachineIds intentionally not set
	})
	require.NoError(t, err)
	require.NotNil(t, created)

	// Verify the created record has an empty string array (not nil)
	assert.NotNil(t, created.AssociatedMachineIds)
	assert.Equal(t, []string{}, created.AssociatedMachineIds)

	// Read it back to ensure the default persists
	retrieved, err := ssd.Get(ctx, nil, created.ID)
	require.NoError(t, err)
	require.NotNil(t, retrieved)

	// Verify the retrieved record also has an empty string array
	assert.NotNil(t, retrieved.AssociatedMachineIds)
	assert.Equal(t, []string{}, retrieved.AssociatedMachineIds)
}
