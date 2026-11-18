// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"sync"
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
	"github.com/uptrace/bun"
	otrace "go.opentelemetry.io/otel/trace"
	"google.golang.org/protobuf/types/known/timestamppb"
)

func TestSkuComponents_Equal(t *testing.T) {
	decoded := &SkuComponents{}
	require.NoError(t, decoded.UnmarshalJSON([]byte(`{"chassis":{"model":"DGX"}}`)))
	fresh := &SkuComponents{SkuComponents: &corev1.SkuComponents{
		Chassis: &corev1.SkuComponentChassis{Model: "DGX"},
	}}

	tests := []struct {
		name  string
		left  *SkuComponents
		right *SkuComponents
		equal bool
	}{
		{name: "nil wrappers are equal", equal: true},
		{name: "nil wrapper equals nil inner", right: &SkuComponents{}, equal: true},
		{name: "nil inner values are equal", left: &SkuComponents{}, right: &SkuComponents{}, equal: true},
		{
			name:  "identical inner protos are equal",
			left:  &SkuComponents{SkuComponents: &corev1.SkuComponents{}},
			right: &SkuComponents{SkuComponents: &corev1.SkuComponents{}},
			equal: true,
		},
		{
			name:  "nil inner does not equal empty proto",
			left:  &SkuComponents{},
			right: &SkuComponents{SkuComponents: &corev1.SkuComponents{}},
		},
		{name: "decoded and fresh protos compare by fields", left: decoded, right: fresh, equal: true},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.equal, tc.left.Equal(tc.right))
			assert.Equal(t, tc.equal, tc.right.Equal(tc.left))
		})
	}
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

func TestSkuSQLDAO_GetForAttachment(t *testing.T) {
	ctx := context.Background()
	dbSession := testSKUInitDB(t)
	defer dbSession.Close()
	testSKUSetupSchema(t, dbSession)
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)
	otherSite := TestBuildSite(t, dbSession, ip, "other-site", user)
	dao := NewSkuDAO(dbSession)
	for _, input := range []SkuCreateInput{
		{SkuID: "last", SiteID: site.ID},
		{SkuID: "first", SiteID: site.ID},
		{SkuID: "foreign", SiteID: otherSite.ID},
	} {
		_, err := dao.Create(ctx, nil, input)
		require.NoError(t, err)
	}

	t.Run("requires a transaction", func(t *testing.T) {
		_, err := dao.GetForAttachment(ctx, nil, site.ID, []string{"first"})
		require.EqualError(t, err, "transaction is required to lock SKU attachments")
	})

	for _, tc := range []struct {
		name string
		ids  []string
		want []string
	}{
		{name: "no requested IDs selects nothing", want: []string{}},
		{
			name: "returns only requested Site SKUs in ID order",
			ids:  []string{"last", "foreign", "missing", "first", "last"},
			want: []string{"first", "last"},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
				skus, err := dao.GetForAttachment(ctx, tx, site.ID, tc.ids)
				if err != nil {
					return err
				}
				ids := make([]string, 0, len(skus))
				for _, sku := range skus {
					ids = append(ids, sku.ID)
				}
				assert.Equal(t, tc.want, ids)
				return nil
			})
			require.NoError(t, err)
		})
	}

	t.Run("locks the smallest ID before later rows", func(t *testing.T) {
		ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
		defer cancel()
		gate, err := db.BeginTx(ctx, dbSession, nil)
		require.NoError(t, err)
		var workers sync.WaitGroup
		defer func() {
			_ = gate.Rollback()
			cancel()
			workers.Wait()
		}()
		var firstID string
		err = gate.GetBunTx().NewSelect().Model((*SKU)(nil)).Column("id").
			Where("sk.id = ?", "first").For("UPDATE").Scan(ctx, &firstID)
		require.NoError(t, err)

		readerReady := make(chan int, 1)
		readerDone := make(chan error, 1)
		workers.Go(func() {
			readerDone <- db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
				// The fixture was inserted in reverse ID order. Force a table
				// scan so an incidental index order cannot hide a missing ORDER BY.
				_, err := tx.GetBunTx().ExecContext(ctx, "SET LOCAL enable_indexscan = off")
				if err != nil {
					return err
				}
				_, err = tx.GetBunTx().ExecContext(ctx, "SET LOCAL enable_bitmapscan = off")
				if err != nil {
					return err
				}
				var pid int
				err = tx.GetBunTx().NewSelect().ColumnExpr("pg_backend_pid()").Scan(ctx, &pid)
				if err != nil {
					return err
				}
				readerReady <- pid
				_, err = dao.GetForAttachment(ctx, tx, site.ID, []string{"last", "first"})
				return err
			})
		})
		var readerPID int
		select {
		case readerPID = <-readerReady:
		case <-ctx.Done():
			t.Fatal("SKU attachment lookup did not start: ", ctx.Err())
		}
		require.Eventually(t, func() bool {
			var blockers int
			err := dbSession.DB.NewSelect().ColumnExpr("cardinality(pg_blocking_pids(?))", readerPID).Scan(ctx, &blockers)
			return err == nil && blockers > 0
		}, 5*time.Second, 10*time.Millisecond, "lookup must wait at the smallest SKU ID")

		// The reader is waiting on first, so it must not already hold last.
		var lastID string
		err = dbSession.DB.NewSelect().Model((*SKU)(nil)).Column("id").
			Where("sk.id = ?", "last").For("UPDATE NOWAIT").Scan(ctx, &lastID)
		require.NoError(t, err, "later SKU must remain unlocked while the first ID is blocked")
		require.Equal(t, "last", lastID)
		require.NoError(t, gate.Commit())
		select {
		case err = <-readerDone:
			require.NoError(t, err)
		case <-ctx.Done():
			t.Fatal("SKU attachment lookup did not finish after release: ", ctx.Err())
		}
	})

	for _, commit := range []bool{true, false} {
		name := "rollback"
		if commit {
			name = "commit"
		}
		t.Run("protects deletion until "+name+" while allowing metadata updates", func(t *testing.T) {
			ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
			defer cancel()
			sku, err := dao.Create(ctx, nil, SkuCreateInput{SkuID: name, SiteID: site.ID})
			require.NoError(t, err)
			tx, err := db.BeginTx(ctx, dbSession, nil)
			require.NoError(t, err)
			var workers sync.WaitGroup
			defer func() {
				_ = tx.Rollback()
				cancel()
				workers.Wait()
			}()
			skus, err := dao.GetForAttachment(ctx, tx, site.ID, []string{sku.ID})
			require.NoError(t, err)
			require.Len(t, skus, 1)

			// KEY SHARE protects the attachment without blocking inventory's
			// ordinary SKU metadata updates.
			updated, err := dao.Update(ctx, nil, SkuUpdateInput{SkuID: sku.ID, Description: cutil.GetPtr("new description")})
			require.NoError(t, err)
			assert.Equal(t, "new description", updated.Description)

			writerReady := make(chan int, 1)
			writerDone := make(chan error, 1)
			workers.Go(func() {
				writerDone <- db.WithTx(ctx, dbSession, func(writer *db.Tx) error {
					var pid int
					err := writer.GetBunTx().NewSelect().ColumnExpr("pg_backend_pid()").Scan(ctx, &pid)
					if err != nil {
						return err
					}
					writerReady <- pid
					return dao.Delete(ctx, writer, sku.ID)
				})
			})
			var writerPID int
			select {
			case writerPID = <-writerReady:
			case <-ctx.Done():
				t.Fatal("SKU deletion did not start: ", ctx.Err())
			}
			require.Eventually(t, func() bool {
				var blockers int
				err := dbSession.DB.NewSelect().ColumnExpr("cardinality(pg_blocking_pids(?))", writerPID).Scan(ctx, &blockers)
				return err == nil && blockers > 0
			}, 5*time.Second, 10*time.Millisecond, "SKU deletion must wait for the attachment transaction")

			if commit {
				require.NoError(t, tx.Commit())
			} else {
				require.NoError(t, tx.Rollback())
			}
			select {
			case err = <-writerDone:
				require.NoError(t, err)
			case <-ctx.Done():
				t.Fatal("SKU deletion did not finish after transaction release: ", ctx.Err())
			}
			_, err = dao.Get(ctx, nil, sku.ID)
			require.ErrorIs(t, err, db.ErrDoesNotExist)
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

type testSkuUpdateHook struct {
	updates int
}

func (h *testSkuUpdateHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if event.Operation() == "UPDATE" {
		h.updates++
	}
	return ctx
}

func (h *testSkuUpdateHook) AfterQuery(context.Context, *bun.QueryEvent) {}

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
	hook := &testSkuUpdateHook{}
	dbSession.DB.AddQueryHook(hook)
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
		noWrite             bool
	}{
		{
			desc:                "no fields preserves the stored values and timestamp",
			input:               SkuUpdateInput{SkuID: created[0].ID},
			expectedDescription: created[0].Description,
			expectedVersion:     created[0].SchemaVersion,
			expectedCreated:     created[0].Created,
			noWrite:             true,
		},
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
			beforeUpdate := time.Date(2025, time.January, 1, 0, 0, 0, 0, time.UTC)
			_, err := dbSession.DB.NewUpdate().Model((*SKU)(nil)).
				Set("updated = ?", beforeUpdate).Where("id = ?", tc.input.SkuID).Exec(ctx)
			require.NoError(t, err)

			updatesBefore := hook.updates
			callStart := time.Now().UTC().Truncate(time.Microsecond)
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
			stored, err := ssd.Get(ctx, nil, tc.input.SkuID)
			require.NoError(t, err)
			assert.Equal(t, got, stored)
			if tc.noWrite {
				assert.Equal(t, updatesBefore, hook.updates, "no fields must not issue an UPDATE")
				assert.True(t, stored.Updated.Equal(beforeUpdate))
			} else {
				assert.Equal(t, updatesBefore+1, hook.updates)
				assert.False(t, stored.Updated.Before(callStart))
			}
			// tracer
			span := otrace.SpanFromContext(ctx)
			assert.True(t, span.SpanContext().IsValid())
		})
	}

	t.Run("conditional writes", func(t *testing.T) {
		row, err := ssd.Create(ctx, nil, SkuCreateInput{
			SkuID: "guarded-sku", SiteID: site.ID, Description: "original description",
		})
		require.NoError(t, err)
		updated := time.Date(2025, time.January, 1, 0, 0, 0, 123_456_000, time.UTC)
		older := updated.Add(-time.Hour)
		newer := updated.Add(time.Hour)
		description := "inventory description"
		cases := []struct {
			name    string
			input   SkuUpdateInput
			wantErr bool
			noWrite bool
		}{
			{
				name:  "matching version writes",
				input: SkuUpdateInput{SkuID: row.ID, ExpectedUpdated: &updated, Description: &description},
			},
			{
				name:    "same schema version with a stale token is rejected",
				input:   SkuUpdateInput{SkuID: row.ID, ExpectedUpdated: &older, SchemaVersion: &row.SchemaVersion},
				wantErr: true,
			},
			{
				name:    "future token is rejected",
				input:   SkuUpdateInput{SkuID: row.ID, ExpectedUpdated: &newer, Description: &description},
				wantErr: true,
			},
			{
				name:    "missing ID does not update a row with the same version",
				input:   SkuUpdateInput{SkuID: "missing-sku", ExpectedUpdated: &updated, Description: &description},
				wantErr: true,
			},
			{
				name:    "missing ID without a token is rejected",
				input:   SkuUpdateInput{SkuID: "missing-sku", Description: &description},
				wantErr: true,
			},
			{
				name:    "stale token without fields performs no write",
				input:   SkuUpdateInput{SkuID: row.ID, ExpectedUpdated: &older},
				noWrite: true,
			},
		}
		for _, tc := range cases {
			t.Run(tc.name, func(t *testing.T) {
				_, err := dbSession.DB.NewUpdate().Model((*SKU)(nil)).
					Set("updated = ?", updated).Set("description = ?", "original description").Where("id = ?", row.ID).Exec(ctx)
				require.NoError(t, err)
				before, err := ssd.Get(ctx, nil, row.ID)
				require.NoError(t, err)
				updatesBefore := hook.updates
				got, err := ssd.Update(ctx, nil, tc.input)
				if tc.wantErr {
					require.ErrorIs(t, err, db.ErrDoesNotExist)
					assert.Nil(t, got)
				} else {
					require.NoError(t, err)
					require.NotNil(t, got)
				}
				stored, err := ssd.Get(ctx, nil, row.ID)
				require.NoError(t, err)
				if tc.noWrite {
					assert.Equal(t, updatesBefore, hook.updates, "a token alone must not issue an UPDATE")
				} else {
					assert.Equal(t, updatesBefore+1, hook.updates)
				}
				if tc.wantErr || tc.noWrite {
					assert.Equal(t, before, stored)
				} else {
					assert.True(t, stored.Updated.After(before.Updated))
					before.Description = description
					before.Updated = stored.Updated
					assert.Equal(t, before, stored)
				}
				if got != nil {
					assert.Equal(t, stored, got)
				}
			})
		}
	})
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

func TestSkuSQLDAO_DeleteIfUnchanged(t *testing.T) {
	ctx := context.Background()
	dbSession := testSKUInitDB(t)
	defer dbSession.Close()
	testSKUSetupSchema(t, dbSession)

	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)
	existing := testSkuCreateSkus(ctx, t, dbSession, site.ID)
	dao := NewSkuDAO(dbSession)
	beforeUpdate := existing[1]
	afterUpdate, err := dao.Update(ctx, nil, SkuUpdateInput{
		SkuID:       beforeUpdate.ID,
		Description: cutil.GetPtr("set through the API"),
	})
	require.NoError(t, err)
	require.True(t, afterUpdate.Updated.After(beforeUpdate.Updated))

	tests := []struct {
		name        string
		id          string
		updated     time.Time
		wantDeleted bool
		want        *SKU
	}{
		{
			name:        "matching version deletes the row",
			id:          existing[0].ID,
			updated:     existing[0].Updated,
			wantDeleted: true,
		},
		{
			name:    "changed version preserves the row",
			id:      beforeUpdate.ID,
			updated: beforeUpdate.Updated,
			want:    afterUpdate,
		},
		{
			name:    "older stored version is also preserved",
			id:      existing[2].ID,
			updated: existing[2].Updated.Add(time.Hour),
			want:    &existing[2],
		},
		{
			name:    "absent row preserves another row with the same timestamp",
			id:      "absent-sku",
			updated: existing[2].Updated,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			deleted, err := dao.DeleteIfUnchanged(ctx, nil, tc.id, tc.updated)
			require.NoError(t, err)
			assert.Equal(t, tc.wantDeleted, deleted)

			stored, err := dao.Get(ctx, nil, tc.id)
			if tc.want == nil {
				assert.ErrorIs(t, err, db.ErrDoesNotExist)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.want, stored)
		})
	}
	stored, err := dao.Get(ctx, nil, existing[2].ID)
	require.NoError(t, err)
	assert.Equal(t, &existing[2], stored)
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
