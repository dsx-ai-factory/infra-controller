// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sku

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/protobuf/proto"
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

func TestManageSku_UpdateSkusInDB(t *testing.T) {
	tests := []struct {
		name string
		run  func(*testing.T)
	}{
		{name: "reconcile create update delete", run: testManageSkuReconcileCreateUpdateDelete},
		{name: "nil components clears existing", run: testManageSkuNilComponentsClearsExisting},
		{name: "failed inventory skips reconciliation", run: testManageSkuInventoryStatusFailedSkipsReconciliation},
		{name: "paged deletion", run: testManageSkuPagedDeletion},
		{name: "update respects freshness", run: testManageSkuUpdateRespectsStaleInventoryThreshold},
		{name: "edit committed before update survives", run: testManageSkuEditBeforeUpdateSurvives},
		{name: "update error allows other updates and a later retry", run: testManageSkuUpdateErrorContinuesAndRetries},
		{name: "deletion respects freshness", run: testManageSkuDeletionRespectsStaleInventoryThreshold},
		{name: "edit committed before deletion survives", run: testManageSkuEditBeforeDeletionSurvives},
		{name: "delete error allows other deletions and a later retry", run: testManageSkuDeleteErrorContinuesAndRetries},
	}
	for _, tc := range tests {
		t.Run(tc.name, tc.run)
	}
}

// testSkuBeforeUpdateHook acts at the SQL write boundary, after any eligibility
// reads, so an unprotected reread cannot satisfy the concurrent-edit test.
type testSkuBeforeUpdateHook struct {
	rowID        string
	beforeUpdate func()
	fired        bool
	err          error
}

func (h *testSkuBeforeUpdateHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if !h.fired && event.Operation() == "UPDATE" && strings.Contains(event.Query, `UPDATE "sku"`) && strings.Contains(event.Query, h.rowID) {
		h.fired = true
		if h.beforeUpdate != nil {
			h.beforeUpdate()
		}
	}
	return ctx
}

func (h *testSkuBeforeUpdateHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
	if event.Operation() == "UPDATE" && strings.Contains(event.Query, `UPDATE "sku"`) && strings.Contains(event.Query, h.rowID) {
		h.err = event.Err
	}
}

func testManageSkuEditBeforeUpdateSurvives(t *testing.T) {
	// Bound the competing write if a regression leaves inventory holding a row lock.
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	_ = config.GetTestConfig()
	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{"FORGE_PROVIDER_ADMIN"})
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	dao := cdbm.NewSkuDAO(dbSession)
	row, err := dao.Create(ctx, nil, cdbm.SkuCreateInput{
		SkuID:         "sku-concurrent-update",
		SiteID:        site.ID,
		Description:   "original description",
		Created:       cutil.GetPtr(time.Date(2025, time.January, 2, 3, 4, 5, 0, time.UTC)),
		SchemaVersion: 1,
		DeviceType:    cutil.GetPtr("gpu-server"),
		Components: &cdbm.SkuComponents{SkuComponents: &corev1.SkuComponents{
			Chassis: &corev1.SkuComponentChassis{Model: "DGX"},
		}},
		AssociatedMachineIds: []string{"machine-original"},
	})
	require.NoError(t, err)
	companion, err := dao.Create(ctx, nil, cdbm.SkuCreateInput{
		SkuID: "sku-continue-update", SiteID: site.ID, Description: "original companion description",
	})
	require.NoError(t, err)
	ageSku(ctx, t, dbSession, row.ID)
	ageSku(ctx, t, dbSession, companion.ID)
	beforeRead, err := dao.Get(ctx, nil, row.ID)
	require.NoError(t, err)
	require.False(t, site.IsTimeWithinStaleInventoryThreshold(beforeRead.Updated))

	var newer *cdbm.SKU
	var writeErr error
	hook := &testSkuBeforeUpdateHook{rowID: row.ID, beforeUpdate: func() {
		newer, writeErr = dao.Update(ctx, nil, cdbm.SkuUpdateInput{
			SkuID:       row.ID,
			Description: cutil.GetPtr("saved REST description"),
		})
	}}
	dbSession.DB.AddQueryHook(hook)
	inventory := &corev1.SkuInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Skus: []*corev1.Sku{
			{
				Id:            row.ID,
				Description:   cutil.GetPtr("older inventory description"),
				Created:       timestamppb.New(row.Created.Add(-time.Hour)),
				SchemaVersion: 2,
				DeviceType:    cutil.GetPtr("compute"),
			},
			{Id: companion.ID, Description: cutil.GetPtr("inventory companion description")},
		},
	}
	ms := NewManageSku(dbSession, cwu.TestTemporalSiteClientPool(t))
	err = ms.UpdateSkusInDB(ctx, site.ID, inventory)
	require.True(t, hook.fired)
	require.NoError(t, hook.err)
	require.NoError(t, writeErr)
	require.NoError(t, err)
	require.NotNil(t, newer)
	require.True(t, newer.Updated.After(beforeRead.Updated))
	stored, err := dao.Get(ctx, nil, row.ID)
	require.NoError(t, err)
	assert.Equal(t, newer, stored, "every field and the REST write timestamp must survive")
	stored, err = dao.Get(ctx, nil, companion.ID)
	require.NoError(t, err)
	assert.Equal(t, "inventory companion description", stored.Description)
}

func testManageSkuUpdateErrorContinuesAndRetries(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()
	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{"FORGE_PROVIDER_ADMIN"})
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	dao := cdbm.NewSkuDAO(dbSession)
	row, err := dao.Create(ctx, nil, cdbm.SkuCreateInput{
		SkuID: "sku-failed-update", SiteID: site.ID, Description: "original description",
	})
	require.NoError(t, err)
	companion, err := dao.Create(ctx, nil, cdbm.SkuCreateInput{
		SkuID: "sku-continue-update", SiteID: site.ID, Description: "original companion description",
	})
	require.NoError(t, err)
	ageSku(ctx, t, dbSession, row.ID)
	ageSku(ctx, t, dbSession, companion.ID)
	beforeUpdate, err := dao.Get(ctx, nil, row.ID)
	require.NoError(t, err)

	_, err = dbSession.DB.ExecContext(ctx, `CREATE OR REPLACE FUNCTION fail_sku_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected database failure'; END $$`)
	require.NoError(t, err)
	defer func() {
		_, cleanupErr := dbSession.DB.ExecContext(ctx, "DROP FUNCTION fail_sku_update() CASCADE")
		require.NoError(t, cleanupErr)
	}()
	_, err = dbSession.DB.ExecContext(ctx, fmt.Sprintf("CREATE TRIGGER fail_update BEFORE UPDATE ON sku FOR EACH ROW WHEN (OLD.id = '%s') EXECUTE FUNCTION fail_sku_update()", row.ID))
	require.NoError(t, err)
	_, err = dao.Update(ctx, nil, cdbm.SkuUpdateInput{
		SkuID: row.ID, ExpectedUpdated: &beforeUpdate.Updated, Description: cutil.GetPtr("inventory description"),
	})
	require.ErrorContains(t, err, "injected database failure")
	require.NotErrorIs(t, err, cdb.ErrDoesNotExist)
	hook := &testSkuBeforeUpdateHook{rowID: row.ID}
	dbSession.DB.AddQueryHook(hook)
	inventory := &corev1.SkuInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Skus: []*corev1.Sku{
			{Id: row.ID, Description: cutil.GetPtr("inventory description")},
			{Id: companion.ID, Description: cutil.GetPtr("inventory companion description")},
		},
	}
	ms := NewManageSku(dbSession, cwu.TestTemporalSiteClientPool(t))
	var logOutput bytes.Buffer
	originalLogger := log.Logger
	log.Logger = zerolog.New(&logOutput).Level(zerolog.ErrorLevel)
	t.Cleanup(func() { log.Logger = originalLogger })
	err = ms.UpdateSkusInDB(ctx, site.ID, inventory)
	log.Logger = originalLogger
	require.NoError(t, err)
	var failure map[string]any
	require.NoError(t, json.Unmarshal(logOutput.Bytes(), &failure))
	assert.Equal(t, "error", failure["level"])
	assert.Equal(t, "UpdateSkusInDB", failure["Activity"])
	assert.Contains(t, failure["error"], "injected database failure")
	require.True(t, hook.fired)
	require.ErrorContains(t, hook.err, "injected database failure")
	stored, err := dao.Get(ctx, nil, row.ID)
	require.NoError(t, err)
	assert.Equal(t, beforeUpdate, stored)
	stored, err = dao.Get(ctx, nil, companion.ID)
	require.NoError(t, err)
	assert.Equal(t, "inventory companion description", stored.Description)

	// A later inventory retries the row once the database accepts writes.
	_, err = dbSession.DB.ExecContext(ctx, "DROP TRIGGER fail_update ON sku")
	require.NoError(t, err)
	require.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inventory))
	stored, err = dao.Get(ctx, nil, row.ID)
	require.NoError(t, err)
	assert.Equal(t, "inventory description", stored.Description)
}

func testManageSkuReconcileCreateUpdateDelete(t *testing.T) {
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
		DeviceType:    cutil.GetPtr("gpu-server"),
		Components:    &corev1.SkuComponents{},
	}}}
	assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv2))

	skus, total, err = ssd.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(100)})
	assert.NoError(t, err)
	assert.Equal(t, 1, total)
	assert.Equal(t, description2, skus[0].Description)
	assert.Equal(t, uint32(5), skus[0].SchemaVersion)
	assert.True(t, created2.Round(time.Microsecond).Equal(skus[0].Created))

	// 3) Equal inventory must not renew the freshness window. Missing Created
	// keeps Core's creation time; missing components and associations are empty.
	ageSku(ctx, t, dbSession, id1)
	beforeNoChange, err := ssd.Get(ctx, nil, id1)
	require.NoError(t, err)
	require.NotNil(t, beforeNoChange.DeviceType)
	require.False(t, site.IsTimeWithinStaleInventoryThreshold(beforeNoChange.Updated))
	for _, tc := range []struct {
		name       string
		deviceType *string
	}{
		{name: "equal device type", deviceType: cutil.GetPtr(*beforeNoChange.DeviceType)},
		{name: "omitted device type", deviceType: nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if tc.deviceType != nil {
				require.NotSame(t, beforeNoChange.DeviceType, tc.deviceType)
			}
			invWithoutCreated := &corev1.SkuInventory{Skus: []*corev1.Sku{{
				Id:            id1,
				Description:   &description2,
				SchemaVersion: 5,
				DeviceType:    tc.deviceType,
			}}}
			require.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, invWithoutCreated))
			stored, err := ssd.Get(ctx, nil, id1)
			require.NoError(t, err)
			assert.Equal(t, beforeNoChange, stored, "unchanged inventory must preserve values and Updated")
		})
	}

	// 4) The unchanged row remains eligible for deletion without aging it again.
	inv3 := &corev1.SkuInventory{Skus: []*corev1.Sku{}}
	assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv3))

	_, total, err = ssd.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(100)})
	assert.NoError(t, err)
	assert.Equal(t, 0, total)
}

func testManageSkuNilComponentsClearsExisting(t *testing.T) {
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

	// Seed populated components so keeping the existing value cannot pass as a clear.
	id := "sku-clear"
	_, err := dbSession.DB.NewInsert().Model(&cdbm.SKU{
		ID: id, SiteID: site.ID,
		Components: &cdbm.SkuComponents{SkuComponents: &corev1.SkuComponents{
			Chassis: &corev1.SkuComponentChassis{Model: "DGX"},
		}},
	}).Exec(ctx)
	require.NoError(t, err)
	ssd := cdbm.NewSkuDAO(dbSession)
	beforeClear, err := ssd.Get(ctx, nil, id)
	require.NoError(t, err)
	require.NotNil(t, beforeClear.Components)
	require.Equal(t, "DGX", beforeClear.Components.GetChassis().GetModel())
	ageSku(ctx, t, dbSession, id)

	ms := NewManageSku(dbSession, cwu.TestTemporalSiteClientPool(t))

	// Send inventory with the same SKU but Components: nil. The activity should
	// translate nil to a non-nil empty wrapper so the DAO actually writes the
	// clear (the DAO skips nil Components fields).
	inv := &corev1.SkuInventory{
		Skus: []*corev1.Sku{{Id: id, Components: nil}},
	}
	assert.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inv))

	got, gerr := ssd.Get(ctx, nil, id)
	require.NoError(t, gerr)
	require.NotNil(t, got.Components)
	assert.True(t, proto.Equal(&corev1.SkuComponents{}, got.Components.SkuComponents))
}

func testManageSkuInventoryStatusFailedSkipsReconciliation(t *testing.T) {
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

func testManageSkuPagedDeletion(t *testing.T) {
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

	// The first-page row must survive because of ItemIds, not the freshness guard.
	firstPageRow, err := ssd.Get(ctx, nil, seed[0])
	require.NoError(t, err)
	require.False(t, site.IsTimeWithinStaleInventoryThreshold(firstPageRow.Updated))

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

func testManageSkuUpdateRespectsStaleInventoryThreshold(t *testing.T) {
	testCases := []struct {
		name            string
		writtenAgo      time.Duration
		wantDescription string
		wantUpdated     bool
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
			wantUpdated:     true,
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

			ssd := cdbm.NewSkuDAO(dbSession)
			beforeInventory, err := ssd.Get(ctx, nil, id)
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

			got, gerr := ssd.Get(ctx, nil, id)
			require.NoError(t, gerr)
			assert.Equal(t, tc.wantDescription, got.Description)
			if tc.wantUpdated {
				assert.True(t, got.Updated.After(beforeInventory.Updated))
			} else {
				assert.Equal(t, beforeInventory.Updated, got.Updated)
			}
		})
	}
}

func testManageSkuDeletionRespectsStaleInventoryThreshold(t *testing.T) {
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

// Run the competing write immediately before the SQL DELETE, after any reads
// reconciliation might use to decide whether the row is still eligible.
type testSkuBeforeDeleteHook struct {
	rowID        string
	beforeDelete func()
	fired        bool
	err          error
}

func (h *testSkuBeforeDeleteHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if !h.fired && event.Operation() == "DELETE" && strings.Contains(event.Query, `FROM "sku"`) && strings.Contains(event.Query, h.rowID) {
		h.fired = true
		if h.beforeDelete != nil {
			h.beforeDelete()
		}
	}
	return ctx
}

func (h *testSkuBeforeDeleteHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
	if event.Operation() == "DELETE" && strings.Contains(event.Query, `FROM "sku"`) && strings.Contains(event.Query, h.rowID) {
		h.err = event.Err
	}
}

func testManageSkuEditBeforeDeletionSurvives(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()
	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{"FORGE_PROVIDER_ADMIN"})
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	dao := cdbm.NewSkuDAO(dbSession)
	row, err := dao.Create(ctx, nil, cdbm.SkuCreateInput{SkuID: "sku-concurrent", SiteID: site.ID})
	require.NoError(t, err)
	companion, err := dao.Create(ctx, nil, cdbm.SkuCreateInput{SkuID: "sku-unchanged", SiteID: site.ID})
	require.NoError(t, err)
	ageSku(ctx, t, dbSession, row.ID)
	ageSku(ctx, t, dbSession, companion.ID)
	beforeRead, err := dao.Get(ctx, nil, row.ID)
	require.NoError(t, err)
	require.False(t, site.IsTimeWithinStaleInventoryThreshold(beforeRead.Updated))

	var newer *cdbm.SKU
	var writeErr error
	hook := &testSkuBeforeDeleteHook{rowID: row.ID, beforeDelete: func() {
		newer, writeErr = dao.Update(ctx, nil, cdbm.SkuUpdateInput{
			SkuID:       row.ID,
			Description: cutil.GetPtr("set through the API"),
		})
	}}
	dbSession.DB.AddQueryHook(hook)
	inventory := &corev1.SkuInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}
	ms := NewManageSku(dbSession, cwu.TestTemporalSiteClientPool(t))
	require.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inventory))
	require.True(t, hook.fired)
	require.NoError(t, hook.err)
	require.NoError(t, writeErr)
	require.NotNil(t, newer)
	require.True(t, newer.Updated.After(beforeRead.Updated))
	stored, err := dao.Get(ctx, nil, row.ID)
	require.NoError(t, err)
	assert.Equal(t, newer, stored, "the API description and Updated must survive")
	_, err = dao.Get(ctx, nil, companion.ID)
	require.ErrorIs(t, err, cdb.ErrDoesNotExist)
}

func testManageSkuDeleteErrorContinuesAndRetries(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()
	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{"FORGE_PROVIDER_ADMIN"})
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	dao := cdbm.NewSkuDAO(dbSession)
	row, err := dao.Create(ctx, nil, cdbm.SkuCreateInput{SkuID: "sku-failed-delete", SiteID: site.ID})
	require.NoError(t, err)
	companion, err := dao.Create(ctx, nil, cdbm.SkuCreateInput{SkuID: "sku-continue-delete", SiteID: site.ID})
	require.NoError(t, err)
	ageSku(ctx, t, dbSession, row.ID)
	ageSku(ctx, t, dbSession, companion.ID)
	// `GetAll` orders by `Created`, so process the failing row first.
	_, err = dbSession.DB.NewUpdate().Model((*cdbm.SKU)(nil)).
		Set("created = ?", companion.Created.Add(-time.Hour)).Where("id = ?", row.ID).Exec(ctx)
	require.NoError(t, err)
	rows, _, err := dao.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{})
	require.NoError(t, err)
	require.Len(t, rows, 2)
	require.Equal(t, row.ID, rows[0].ID)
	require.Equal(t, companion.ID, rows[1].ID)

	_, err = dbSession.DB.ExecContext(ctx, `CREATE OR REPLACE FUNCTION fail_sku_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected database failure'; END $$`)
	require.NoError(t, err)
	defer func() {
		_, cleanupErr := dbSession.DB.ExecContext(ctx, "DROP FUNCTION fail_sku_delete() CASCADE")
		require.NoError(t, cleanupErr)
	}()
	_, err = dbSession.DB.ExecContext(ctx, fmt.Sprintf("CREATE TRIGGER fail_delete BEFORE DELETE ON sku FOR EACH ROW WHEN (OLD.id = '%s') EXECUTE FUNCTION fail_sku_delete()", row.ID))
	require.NoError(t, err)
	deleted, err := dao.DeleteIfUnchanged(ctx, nil, row.ID, rows[0].Updated)
	require.False(t, deleted)
	require.ErrorContains(t, err, "injected database failure")
	hook := &testSkuBeforeDeleteHook{rowID: row.ID}
	dbSession.DB.AddQueryHook(hook)
	inventory := &corev1.SkuInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}
	ms := NewManageSku(dbSession, cwu.TestTemporalSiteClientPool(t))
	require.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inventory))
	require.True(t, hook.fired)
	require.ErrorContains(t, hook.err, "injected database failure")
	stored, err := dao.Get(ctx, nil, row.ID)
	require.NoError(t, err)
	assert.Equal(t, &rows[0], stored)
	_, err = dao.Get(ctx, nil, companion.ID)
	require.ErrorIs(t, err, cdb.ErrDoesNotExist)

	// A later inventory retries the row once the database accepts writes.
	_, err = dbSession.DB.ExecContext(ctx, "DROP TRIGGER fail_delete ON sku")
	require.NoError(t, err)
	require.NoError(t, ms.UpdateSkusInDB(ctx, site.ID, inventory))
	_, err = dao.Get(ctx, nil, row.ID)
	require.ErrorIs(t, err, cdb.ErrDoesNotExist)
}
