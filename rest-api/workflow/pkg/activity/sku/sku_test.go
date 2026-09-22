// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sku

import (
	"context"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// ageSku backdates a SKU's write time past the staleness threshold. Both guards in the activity
// read Updated, and the DAO rewrites it on every write, so a step that follows an earlier write
// is otherwise deferred rather than applied.
func ageSku(ctx context.Context, t *testing.T, dbSession *cdb.Session, id string) {
	t.Helper()

	_, err := dbSession.DB.NewUpdate().
		Model((*cdbm.SKU)(nil)).
		Set("updated = ?", time.Now().Add(-2*cutil.DefaultInventoryReceiptInterval)).
		Where("id = ?", id).
		Exec(ctx)
	require.NoError(t, err)
}

func TestManageSku_Reconcile_CreateUpdateDelete(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	// Build basic graph: provider, tenant, site
	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, site)

	ms := NewManageSku(dbSession, cwu.TestTemporalSiteClientPool(t))

	// 1) Create: inventory contains one sku not in DB
	id1 := "sku-1"
	description1 := "Initial SKU description"
	created1 := time.Date(2025, time.January, 2, 3, 4, 5, 678_901_234, time.UTC)
	inv1 := &corev1.SkuInventory{
		Skus: []*corev1.Sku{{Id: id1, Description: &description1, Created: timestamppb.New(created1), SchemaVersion: 4, Components: &corev1.SkuComponents{}}},
	}
	assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv1))

	ssd := cdbm.NewSkuDAO(dbSession)
	skus, total, err := ssd.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(100)})
	assert.NoError(t, err)
	assert.Equal(t, 1, total)
	assert.Equal(t, id1, skus[0].ID)
	assert.Equal(t, description1, skus[0].Description)
	assert.Equal(t, uint32(4), skus[0].SchemaVersion)
	assert.True(t, created1.Round(time.Microsecond).Equal(skus[0].Created))
	if skus[0].Components == nil {
		t.Fatalf("expected SkuData to be set")
	}

	// 2) Update: same id, ensure still one record
	ageSku(ctx, t, dbSession, id1)
	description2 := "Updated SKU description"
	created2 := time.Date(2024, time.December, 1, 2, 3, 4, 567_890_123, time.UTC)
	inv2 := &corev1.SkuInventory{Skus: []*corev1.Sku{{
		Id:            id1,
		Description:   &description2,
		Created:       timestamppb.New(created2),
		SchemaVersion: 5,
		Components:    &corev1.SkuComponents{},
	}}}
	assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv2))

	skus, total, err = ssd.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(100)})
	assert.NoError(t, err)
	assert.Equal(t, 1, total)
	assert.Equal(t, description2, skus[0].Description)
	assert.Equal(t, uint32(5), skus[0].SchemaVersion)
	assert.True(t, created2.Round(time.Microsecond).Equal(skus[0].Created))

	// 3) Missing source timestamp preserves the stored creation time.
	ageSku(ctx, t, dbSession, id1)
	invWithoutCreated := &corev1.SkuInventory{Skus: []*corev1.Sku{{
		Id:            id1,
		Description:   &description2,
		SchemaVersion: 5,
		Components:    &corev1.SkuComponents{},
	}}}
	assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, invWithoutCreated))
	skus, total, err = ssd.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(100)})
	assert.NoError(t, err)
	assert.Equal(t, 1, total)
	assert.True(t, created2.Round(time.Microsecond).Equal(skus[0].Created))

	// 4) Delete: send empty inventory, final page implied
	ageSku(ctx, t, dbSession, id1)
	inv3 := &corev1.SkuInventory{Skus: []*corev1.Sku{}}
	assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv3))

	_, total, err = ssd.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(100)})
	assert.NoError(t, err)
	assert.Equal(t, 0, total)
}

func TestManageSku_NilComponents_ClearsExisting(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)

	// Seed a SKU with non-nil Components. Age it so the update runs, since the seeded value
	// would otherwise satisfy the assertion below without the clear ever happening.
	id := "sku-clear"
	_, err := dbSession.DB.NewInsert().Model(&cdbm.SKU{ID: id, SiteID: site.ID, Components: &cdbm.SkuComponents{SkuComponents: &corev1.SkuComponents{}}}).Exec(ctx)
	assert.NoError(t, err)
	ageSku(ctx, t, dbSession, id)

	ms := NewManageSku(dbSession, cwu.TestTemporalSiteClientPool(t))

	// Send inventory with the same SKU but Components: nil. The activity should
	// translate nil to a non-nil empty wrapper so the DAO actually writes the
	// clear (the DAO skips nil Components fields).
	inv := &corev1.SkuInventory{
		Skus: []*corev1.Sku{{Id: id, Components: nil}},
	}
	assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv))

	ssd := cdbm.NewSkuDAO(dbSession)
	got, gerr := ssd.Get(ctx, nil, id)
	assert.NoError(t, gerr)
	if got.Components == nil || got.Components.SkuComponents == nil {
		t.Fatalf("expected Components to be a non-nil empty wrapper after clear, got %+v", got.Components)
	}
}

func TestManageSku_InventoryStatusFailed_Skip(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	// Build site
	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)

	// Seed one SKU (ensure SiteID is set)
	_, err := dbSession.DB.NewInsert().Model(&cdbm.SKU{ID: "sku-seed", SiteID: site.ID, Components: &cdbm.SkuComponents{}}).Exec(ctx)
	assert.NoError(t, err)

	ms := NewManageSku(dbSession, cwu.TestTemporalSiteClientPool(t))

	inv := &corev1.SkuInventory{
		Skus:            []*corev1.Sku{{Id: "sku-other"}},
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
	}

	assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv))

	// Ensure original remains and no changes happened
	ssd := cdbm.NewSkuDAO(dbSession)
	_, total, err := ssd.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(100)})
	assert.NoError(t, err)
	assert.Equal(t, 1, total)
}

func TestManageSku_PagedDeletion(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	// Build site
	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)

	// Seed three SKUs (ensure SiteID is set). Backdate them past the staleness threshold so the
	// deletion sweep is not deferred.
	ssd := cdbm.NewSkuDAO(dbSession)
	seed := []string{"sku-1", "sku-2", "sku-3"}
	for _, id := range seed {
		_, err := dbSession.DB.NewInsert().Model(&cdbm.SKU{ID: id, SiteID: site.ID, Components: &cdbm.SkuComponents{}}).Exec(ctx)
		assert.NoError(t, err)
		ageSku(ctx, t, dbSession, id)
	}

	ms := NewManageSku(dbSession, cwu.TestTemporalSiteClientPool(t))

	// First page: report only first ID, no deletion should occur yet
	inv1 := &corev1.SkuInventory{
		Skus:            []*corev1.Sku{{Id: seed[0]}},
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		InventoryPage:   &corev1.InventoryPage{CurrentPage: 1, TotalPages: 2, PageSize: 1, TotalItems: 2, ItemIds: []string{seed[0], seed[1]}},
	}
	assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv1))
	_, total, err := ssd.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(100)})
	assert.NoError(t, err)
	assert.Equal(t, 3, total)

	// Last page: report only second ID, third should be deleted
	inv2 := &corev1.SkuInventory{
		Skus:            []*corev1.Sku{{Id: seed[1]}},
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		InventoryPage:   &corev1.InventoryPage{CurrentPage: 2, TotalPages: 2, PageSize: 1, TotalItems: 2, ItemIds: []string{seed[0], seed[1]}},
	}
	assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv2))

	got, total, err := ssd.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(100)})
	assert.NoError(t, err)
	assert.Equal(t, 2, total)
	// Remaining should be sku-1 and sku-2
	found := map[string]bool{}
	for _, sk := range got {
		found[sk.ID] = true
	}
	assert.True(t, found[seed[0]])
	assert.True(t, found[seed[1]])
	assert.False(t, found[seed[2]])
}

func TestManageSku_UpdateRespectsStaleInventoryThreshold(t *testing.T) {
	testCases := []struct {
		name            string
		writtenAgo      time.Duration
		wantDescription string
	}{
		{
			// The API can write a SKU, so a row written after the Site collected this inventory
			// holds changes the snapshot cannot know about.
			name:            "a recently written SKU keeps its stored values",
			writtenAgo:      time.Second,
			wantDescription: "set through the API",
		},
		{
			name:            "a SKU untouched for longer than the interval takes the reported values",
			writtenAgo:      2 * cutil.DefaultInventoryReceiptInterval,
			wantDescription: "reported by the Site",
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			_ = config.GetTestConfig()

			dbSession := cwu.TestInitDB(t)
			defer dbSession.Close()
			cwu.TestSetupSchema(t, dbSession)

			ipOrg := "test-ip-org"
			ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
			ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
			ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
			site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)

			id := "sku-updated"
			_, err := dbSession.DB.NewInsert().Model(&cdbm.SKU{
				ID:          id,
				SiteID:      site.ID,
				Description: "set through the API",
				Components:  &cdbm.SkuComponents{},
			}).Exec(ctx)
			require.NoError(t, err)

			_, err = dbSession.DB.NewUpdate().
				Model((*cdbm.SKU)(nil)).
				Set("updated = ?", time.Now().Add(-tc.writtenAgo)).
				Where("id = ?", id).
				Exec(ctx)
			require.NoError(t, err)

			ms := NewManageSku(dbSession, cwu.TestTemporalSiteClientPool(t))

			// The Site reports a competing description for the same SKU.
			reportedDescription := "reported by the Site"
			inv := &corev1.SkuInventory{
				Skus: []*corev1.Sku{{
					Id:          id,
					Description: &reportedDescription,
					Components:  &corev1.SkuComponents{},
				}},
				InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			}
			require.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv))

			ssd := cdbm.NewSkuDAO(dbSession)
			got, gerr := ssd.Get(ctx, nil, id)
			require.NoError(t, gerr)
			assert.Equal(t, tc.wantDescription, got.Description)
		})
	}
}

func TestManageSku_DeletionRespectsStaleInventoryThreshold(t *testing.T) {
	testCases := []struct {
		name       string
		writtenAgo time.Duration
		// createdAgo defaults to writtenAgo, so a case only sets it to separate Core's creation
		// time from the row's write time.
		createdAgo     time.Duration
		intervalSecs   *int
		expectSurvives bool
	}{
		{
			name:           "written within the default threshold survives",
			writtenAgo:     time.Second,
			expectSurvives: true,
		},
		{
			name:       "written past the default threshold is deleted",
			writtenAgo: 2 * cutil.DefaultInventoryReceiptInterval,
		},
		{
			// Older than the default threshold, still inside the reported one.
			name:           "written within a longer reported interval survives",
			writtenAgo:     4 * time.Minute,
			intervalSecs:   cutil.GetPtr(300),
			expectSurvives: true,
		},
		{
			name:         "written past a shorter reported interval is deleted",
			writtenAgo:   time.Minute,
			intervalSecs: cutil.GetPtr(1),
		},
		{
			// Created stays at Core's timestamp for a SKU Cloud only just learned about, so
			// anchoring on it would delete this row.
			name:           "an old Core creation time does not expose a freshly written row",
			writtenAgo:     time.Second,
			createdAgo:     90 * 24 * time.Hour,
			expectSurvives: true,
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			_ = config.GetTestConfig()

			dbSession := cwu.TestInitDB(t)
			defer dbSession.Close()
			cwu.TestSetupSchema(t, dbSession)

			ipOrg := "test-ip-org"
			ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
			ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
			ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
			site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)

			if tc.intervalSecs != nil {
				stDAO := cdbm.NewSiteDAO(dbSession)
				_, uerr := stDAO.Update(ctx, nil, cdbm.SiteUpdateInput{
					SiteID:                   site.ID,
					InventoryIntervalSeconds: tc.intervalSecs,
				})
				assert.NoError(t, uerr)
			}

			createdAgo := tc.createdAgo
			if createdAgo == 0 {
				createdAgo = tc.writtenAgo
			}

			// The DAO rewrites Updated on insert, so set it afterwards to place the row's write
			// time where the case wants it.
			id := "sku-guarded"
			_, err := dbSession.DB.NewInsert().Model(&cdbm.SKU{
				ID:         id,
				SiteID:     site.ID,
				Created:    time.Now().Add(-createdAgo),
				Components: &cdbm.SkuComponents{},
			}).Exec(ctx)
			assert.NoError(t, err)

			_, err = dbSession.DB.NewUpdate().
				Model((*cdbm.SKU)(nil)).
				Set("updated = ?", time.Now().Add(-tc.writtenAgo)).
				Where("id = ?", id).
				Exec(ctx)
			require.NoError(t, err)

			ms := NewManageSku(dbSession, cwu.TestTemporalSiteClientPool(t))

			// A successful final page that omits the seeded SKU entirely.
			inv := &corev1.SkuInventory{
				Skus:            []*corev1.Sku{},
				InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				InventoryPage:   &corev1.InventoryPage{CurrentPage: 1, TotalPages: 1, PageSize: 1, TotalItems: 0},
			}
			assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv))

			ssd := cdbm.NewSkuDAO(dbSession)
			_, total, gerr := ssd.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(100)})
			assert.NoError(t, gerr)
			if tc.expectSurvives {
				assert.Equal(t, 1, total, "SKU newer than the threshold should not be deleted")
			} else {
				assert.Equal(t, 0, total, "SKU older than the threshold should be deleted")
			}
		})
	}
}
