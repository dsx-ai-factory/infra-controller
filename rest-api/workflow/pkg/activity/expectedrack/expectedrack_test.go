// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrack

import (
	"context"
	"fmt"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"go.temporal.io/sdk/testsuite"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/extra/bundebug"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// testTemporalSiteClientPool builds a site client pool for activity tests.
func testTemporalSiteClientPool(t *testing.T) *sc.ClientPool {
	keyPath, certPath := config.SetupTestCerts(t)
	defer os.Remove(keyPath)
	defer os.Remove(certPath)

	cfg := config.NewConfig()
	cfg.SetTemporalCertPath(certPath)
	cfg.SetTemporalKeyPath(keyPath)
	cfg.SetTemporalCaPath(certPath)

	tcfg, err := cfg.GetTemporalConfig()
	assert.NoError(t, err)

	tSiteClientPool := sc.NewClientPool(tcfg)
	return tSiteClientPool
}

func testExpectedRackInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testExpectedRackSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create ExpectedRack table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.ExpectedRack)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)

	// Add deferrable unique constraint for (rack_id, site_id) which is required by the
	// reconciliation logic to prevent duplicate ExpectedRack rows per Site.
	_, err = dbSession.DB.Exec("ALTER TABLE expected_rack DROP CONSTRAINT IF EXISTS expected_rack_rack_id_site_id_key")
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("ALTER TABLE expected_rack ADD CONSTRAINT expected_rack_rack_id_site_id_key UNIQUE (rack_id, site_id) DEFERRABLE INITIALLY DEFERRED")
	assert.Nil(t, err)
}

type testExpectedRackInventoryContextKey struct{}

type testExpectedRackInventoryHook struct {
	afterRead    func()
	afterUpdate  func(error)
	beforeCommit func()
	rowID        uuid.UUID
}

func (h *testExpectedRackInventoryHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if ctx.Value(testExpectedRackInventoryContextKey{}) == h && event.Operation() == "COMMIT" && h.beforeCommit != nil {
		beforeCommit := h.beforeCommit
		h.beforeCommit = nil
		beforeCommit()
	}
	return ctx
}

func (h *testExpectedRackInventoryHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
	if ctx.Value(testExpectedRackInventoryContextKey{}) != h {
		return
	}
	if event.Operation() == "UPDATE" && h.afterUpdate != nil && strings.Contains(event.Query, `UPDATE "expected_rack"`) &&
		(h.rowID == uuid.Nil || strings.Contains(event.Query, h.rowID.String())) {
		h.afterUpdate(event.Err)
	}
	if h.afterRead == nil || event.Err != nil || event.Operation() != "SELECT" ||
		strings.HasPrefix(event.Query, "SELECT count(") || !strings.Contains(event.Query, `FROM "expected_rack"`) {
		return
	}
	if h.rowID != uuid.Nil && !strings.Contains(event.Query, h.rowID.String()) {
		return
	}
	afterRead := h.afterRead
	h.afterRead = nil
	afterRead()
}

type testExpectedRackBeforeDeleteHook struct {
	rowID        uuid.UUID
	beforeDelete func()
	fired        bool
	err          error
}

func (h *testExpectedRackBeforeDeleteHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if !h.fired && event.Operation() == "DELETE" && strings.Contains(event.Query, `FROM "expected_rack"`) && strings.Contains(event.Query, h.rowID.String()) {
		h.fired = true
		if h.beforeDelete != nil {
			h.beforeDelete()
		}
	}
	return ctx
}

func (h *testExpectedRackBeforeDeleteHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
	if event.Operation() == "DELETE" && strings.Contains(event.Query, `FROM "expected_rack"`) && strings.Contains(event.Query, h.rowID.String()) {
		h.err = event.Err
	}
}

func TestManageExpectedRack_UpdateExpectedRacksInDB(t *testing.T) {
	cases := []struct {
		name string
		run  func(*testing.T)
	}{
		{name: "matching inventory preserves Updated", run: testExpectedRackInventoryNoChange},
		{name: "recent write survives missing inventory", run: testExpectedRackInventoryRecentWrite},
		{name: "recent write survives conflicting inventory", run: testExpectedRackInventoryRecentReportedWrite},
		{
			name: "nil metadata clears stored strings and labels while preserving an omitted group",
			run: func(t *testing.T) {
				ctx := context.Background()
				dbSession := testExpectedRackInitDB(t)
				defer dbSession.Close()
				testExpectedRackSetupSchema(t, dbSession)
				ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{"test-provider-org"}, []string{"FORGE_PROVIDER_ADMIN"})
				ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-provider-org", ipu)
				site := cwu.TestBuildSite(t, dbSession, ip, "clear-metadata-site", cdbm.SiteStatusRegistered, nil, ipu)
				erDAO := cdbm.NewExpectedRackDAO(dbSession)
				row, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
					ExpectedRackID: uuid.New(),
					SiteID:         site.ID,
					RackID:         "rack-clear-metadata",
					RackProfileID:  "profile-original",
					RackGroupID:    cutil.GetPtr("group-original"),
					Name:           "original-name",
					Description:    "original-description",
					Labels:         map[string]string{"location": "original"},
					CreatedBy:      ipu.ID,
				})
				require.NoError(t, err)
				cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedRack)(nil))

				inventory := &corev1.ExpectedRackInventory{
					ExpectedRacks: []*corev1.ExpectedRack{{
						RackId:        &corev1.RackId{Id: row.RackID},
						RackProfileId: &corev1.RackProfileId{Id: "profile-reported"},
						Metadata:      nil,
					}},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				}
				mer := ManageExpectedRack{dbSession: dbSession}
				require.NoError(t, mer.UpdateExpectedRacksInDB(ctx, site.ID, inventory))

				persisted, err := erDAO.Get(ctx, nil, row.ID, nil, false)
				require.NoError(t, err)
				assert.Empty(t, persisted.Name)
				assert.Empty(t, persisted.Description)
				assert.Empty(t, persisted.Labels)
				assert.Equal(t, cutil.GetPtr("group-original"), persisted.RackGroupID)
				assert.Equal(t, "profile-reported", persisted.RackProfileID)
			},
		},
		{
			name: "REST write after inventory read preserves the entire newer row",
			run: func(t *testing.T) {
				ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
				defer cancel()
				dbSession := testExpectedRackInitDB(t)
				defer dbSession.Close()
				testExpectedRackSetupSchema(t, dbSession)
				ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{"test-provider-org"}, []string{"FORGE_PROVIDER_ADMIN"})
				ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-provider-org", ipu)
				site := cwu.TestBuildSite(t, dbSession, ip, "concurrent-update-site", cdbm.SiteStatusRegistered, nil, ipu)
				erDAO := cdbm.NewExpectedRackDAO(dbSession)
				row, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
					ExpectedRackID: uuid.New(),
					SiteID:         site.ID,
					RackID:         "concurrent-update",
					RackProfileID:  "profile-original",
					RackGroupID:    cutil.GetPtr("group-original"),
					Name:           "original-name",
					Description:    "original-description",
					Labels:         map[string]string{"location": "original"},
					CreatedBy:      ipu.ID,
				})
				require.NoError(t, err)
				companion, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
					ExpectedRackID: uuid.New(),
					SiteID:         site.ID,
					RackID:         "continue-update",
					RackProfileID:  "profile-original",
					Name:           "original-companion",
					CreatedBy:      ipu.ID,
				})
				require.NoError(t, err)
				cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedRack)(nil))
				companion, err = erDAO.Get(ctx, nil, companion.ID, nil, false)
				require.NoError(t, err)
				beforeRead, err := erDAO.Get(ctx, nil, row.ID, nil, false)
				require.NoError(t, err)
				require.False(t, site.IsTimeWithinStaleInventoryThreshold(beforeRead.Updated))

				// Commit the same metadata write REST makes after inventory has
				// cached the old row, before it decides whether to update it.
				var newer *cdbm.ExpectedRack
				var writeErr error
				fired := false
				hook := &testExpectedRackInventoryHook{afterRead: func() {
					fired = true
					newer, writeErr = cdb.WithTxResult(ctx, dbSession, func(tx *cdb.Tx) (*cdbm.ExpectedRack, error) {
						return erDAO.Update(ctx, tx, cdbm.ExpectedRackUpdateInput{
							ExpectedRackID: row.ID,
							Name:           cutil.GetPtr("REST-name"),
							Description:    cutil.GetPtr("REST-description"),
							Labels:         map[string]string{"location": "REST"},
						})
					})
				}}
				dbSession.DB.AddQueryHook(hook)
				inventory := &corev1.ExpectedRackInventory{
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					ExpectedRacks: []*corev1.ExpectedRack{{
						RackId:        &corev1.RackId{Id: row.RackID},
						RackProfileId: &corev1.RackProfileId{Id: "profile-inventory"},
						RackGroupId:   &corev1.RackGroupId{Id: "group-inventory"},
						Metadata: &corev1.Metadata{
							Name:        "inventory-name",
							Description: "inventory-description",
							Labels:      []*corev1.Label{{Key: "location", Value: cutil.GetPtr("inventory")}},
						},
					}, {
						RackId:        &corev1.RackId{Id: companion.RackID},
						RackProfileId: &corev1.RackProfileId{Id: companion.RackProfileID},
						Metadata:      &corev1.Metadata{Name: "inventory-companion", Description: "inventory-companion-description"},
					}},
				}
				mer := ManageExpectedRack{dbSession: dbSession}
				err = mer.UpdateExpectedRacksInDB(context.WithValue(ctx, testExpectedRackInventoryContextKey{}, hook), site.ID, inventory)
				require.NoError(t, err)
				require.True(t, fired)
				require.NoError(t, writeErr)
				require.True(t, newer.Updated.After(beforeRead.Updated))
				stored, err := erDAO.Get(ctx, nil, row.ID, nil, false)
				require.NoError(t, err)
				assert.Equal(t, newer, stored, "the complete REST row, including its Updated timestamp, must survive")
				storedCompanion, err := erDAO.Get(ctx, nil, companion.ID, nil, false)
				require.NoError(t, err)
				assert.Equal(t, "inventory-companion", storedCompanion.Name)
				assert.True(t, storedCompanion.Updated.After(companion.Updated))
			},
		},
		{
			name: "REST write waits until inventory commits",
			run: func(t *testing.T) {
				ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
				defer cancel()
				dbSession := testExpectedRackInitDB(t)
				defer dbSession.Close()
				testExpectedRackSetupSchema(t, dbSession)
				ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{"test-provider-org"}, []string{"FORGE_PROVIDER_ADMIN"})
				ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-provider-org", ipu)
				site := cwu.TestBuildSite(t, dbSession, ip, "locked-update-site", cdbm.SiteStatusRegistered, nil, ipu)
				erDAO := cdbm.NewExpectedRackDAO(dbSession)
				row, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
					ExpectedRackID: uuid.New(),
					SiteID:         site.ID,
					RackID:         "locked-update",
					RackProfileID:  "profile-original",
					Name:           "original-name",
					Description:    "original-description",
					CreatedBy:      ipu.ID,
				})
				require.NoError(t, err)
				cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedRack)(nil))

				readReached, commitReached := make(chan struct{}), make(chan struct{})
				continueRead, continueCommit := make(chan struct{}), make(chan struct{})
				releaseRead := sync.OnceFunc(func() { close(continueRead) })
				releaseCommit := sync.OnceFunc(func() { close(continueCommit) })
				var workers sync.WaitGroup
				defer func() {
					releaseRead()
					releaseCommit()
					cancel()
					workers.Wait()
				}()
				inventoryWritten := false
				hook := &testExpectedRackInventoryHook{
					rowID: row.ID,
					afterUpdate: func(err error) {
						inventoryWritten = err == nil
					},
					afterRead: func() {
						close(readReached)
						select {
						case <-continueRead:
						case <-ctx.Done():
						}
					},
					beforeCommit: func() {
						close(commitReached)
						select {
						case <-continueCommit:
						case <-ctx.Done():
						}
					},
				}
				dbSession.DB.AddQueryHook(hook)
				inventory := &corev1.ExpectedRackInventory{
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					ExpectedRacks: []*corev1.ExpectedRack{{
						RackId:        &corev1.RackId{Id: row.RackID},
						RackProfileId: &corev1.RackProfileId{Id: row.RackProfileID},
						Metadata:      &corev1.Metadata{Name: "inventory-name", Description: "inventory-description"},
					}},
				}
				mer := ManageExpectedRack{dbSession: dbSession}
				var activityErr, writerErr error
				workers.Go(func() {
					activityErr = mer.UpdateExpectedRacksInDB(context.WithValue(ctx, testExpectedRackInventoryContextKey{}, hook), site.ID, inventory)
				})
				select {
				case <-readReached:
				case <-ctx.Done():
					t.Fatal("inventory did not reach its row reload: ", ctx.Err())
				}

				writerReady := make(chan int, 1)
				workers.Go(func() {
					writerErr = cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
						var pid int
						err := tx.GetBunTx().NewSelect().ColumnExpr("pg_backend_pid()").Scan(ctx, &pid)
						if err != nil {
							return err
						}
						writerReady <- pid
						_, err = erDAO.Update(ctx, tx, cdbm.ExpectedRackUpdateInput{
							ExpectedRackID: row.ID,
							Name:           cutil.GetPtr("REST-name"),
						})
						return err
					})
				})
				var writerPID int
				select {
				case writerPID = <-writerReady:
				case <-ctx.Done():
					t.Fatal("REST writer did not start: ", ctx.Err())
				}
				assertWriterBlocked := func() {
					t.Helper()
					require.Eventually(t, func() bool {
						var blockers int
						err := dbSession.DB.NewSelect().ColumnExpr("cardinality(pg_blocking_pids(?))", writerPID).Scan(ctx, &blockers)
						return err == nil && blockers > 0
					}, 5*time.Second, 10*time.Millisecond, "REST write did not wait for inventory")
				}
				assertWriterBlocked()
				releaseRead()
				select {
				case <-commitReached:
				case <-ctx.Done():
					t.Fatal("inventory did not reach commit: ", ctx.Err())
				}
				require.True(t, inventoryWritten, "inventory must write before committing its row lock")
				// The lock must still protect the row after inventory writes it.
				assertWriterBlocked()
				releaseCommit()
				workers.Wait()
				require.NoError(t, activityErr)
				require.NoError(t, writerErr)
				stored, err := erDAO.Get(ctx, nil, row.ID, nil, false)
				require.NoError(t, err)
				assert.Equal(t, "REST-name", stored.Name)
				assert.Equal(t, "inventory-description", stored.Description)
			},
		},
		{
			name: "update failure rolls back the row, continues, and retries on later inventory",
			run: func(t *testing.T) {
				ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
				defer cancel()
				dbSession := testExpectedRackInitDB(t)
				defer dbSession.Close()
				testExpectedRackSetupSchema(t, dbSession)
				ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{"test-provider-org"}, []string{"FORGE_PROVIDER_ADMIN"})
				ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-provider-org", ipu)
				site := cwu.TestBuildSite(t, dbSession, ip, "failed-update-site", cdbm.SiteStatusRegistered, nil, ipu)
				erDAO := cdbm.NewExpectedRackDAO(dbSession)
				row, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
					ExpectedRackID: uuid.New(),
					SiteID:         site.ID,
					RackID:         "failed-update",
					RackProfileID:  "profile-original",
					RackGroupID:    cutil.GetPtr("group-original"),
					Name:           "original-name",
					Description:    "original-description",
					Labels:         map[string]string{"location": "original"},
					CreatedBy:      ipu.ID,
				})
				require.NoError(t, err)
				companion, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
					ExpectedRackID: uuid.New(),
					SiteID:         site.ID,
					RackID:         "continue-after-failure",
					RackProfileID:  "profile-original",
					CreatedBy:      ipu.ID,
				})
				require.NoError(t, err)
				cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedRack)(nil))
				before, err := erDAO.Get(ctx, nil, row.ID, nil, false)
				require.NoError(t, err)

				// Fail the second DAO UPDATE, after metadata and `Updated` were written.
				_, err = dbSession.DB.ExecContext(ctx, `CREATE FUNCTION fail_expected_rack_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected rack-group update failure'; END $$`)
				require.NoError(t, err)
				defer func() {
					_, cleanupErr := dbSession.DB.ExecContext(ctx, "DROP FUNCTION fail_expected_rack_update() CASCADE")
					require.NoError(t, cleanupErr)
				}()
				_, err = dbSession.DB.ExecContext(ctx, fmt.Sprintf("CREATE TRIGGER fail_update BEFORE UPDATE OF rack_group_id ON expected_rack FOR EACH ROW WHEN (OLD.id = '%s') EXECUTE FUNCTION fail_expected_rack_update()", row.ID))
				require.NoError(t, err)
				var updateErr error
				successfulUpdates := 0
				hook := &testExpectedRackInventoryHook{
					rowID: row.ID,
					afterUpdate: func(err error) {
						if err != nil {
							updateErr = err
							return
						}
						successfulUpdates++
					},
				}
				dbSession.DB.AddQueryHook(hook)
				inventory := &corev1.ExpectedRackInventory{
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					ExpectedRacks: []*corev1.ExpectedRack{{
						RackId:        &corev1.RackId{Id: row.RackID},
						RackProfileId: &corev1.RackProfileId{Id: "profile-inventory"},
						RackGroupId:   &corev1.RackGroupId{Id: "group-inventory"},
						Metadata: &corev1.Metadata{
							Name:        "inventory-name",
							Description: "inventory-description",
							Labels:      []*corev1.Label{{Key: "location", Value: cutil.GetPtr("inventory")}},
						},
					}, {
						RackId:        &corev1.RackId{Id: companion.RackID},
						RackProfileId: &corev1.RackProfileId{Id: companion.RackProfileID},
						Metadata:      &corev1.Metadata{Name: "inventory-companion", Description: "inventory-companion-description"},
					}},
				}
				mer := ManageExpectedRack{dbSession: dbSession}
				inventoryCtx := context.WithValue(ctx, testExpectedRackInventoryContextKey{}, hook)
				require.NoError(t, mer.UpdateExpectedRacksInDB(inventoryCtx, site.ID, inventory))
				require.Equal(t, 1, successfulUpdates)
				require.ErrorContains(t, updateErr, "injected rack-group update failure")
				stored, err := erDAO.Get(ctx, nil, row.ID, nil, false)
				require.NoError(t, err)
				assert.Equal(t, before, stored, "the failed group update must roll back metadata and Updated too")
				storedCompanion, err := erDAO.Get(ctx, nil, companion.ID, nil, false)
				require.NoError(t, err)
				assert.Equal(t, "inventory-companion", storedCompanion.Name)

				_, err = dbSession.DB.ExecContext(ctx, "DROP TRIGGER fail_update ON expected_rack")
				require.NoError(t, err)
				require.NoError(t, mer.UpdateExpectedRacksInDB(inventoryCtx, site.ID, inventory))
				stored, err = erDAO.Get(ctx, nil, row.ID, nil, false)
				require.NoError(t, err)
				assert.Equal(t, "profile-inventory", stored.RackProfileID)
				assert.Equal(t, cutil.GetPtr("group-inventory"), stored.RackGroupID)
				assert.Equal(t, "inventory-name", stored.Name)
				assert.Equal(t, "inventory-description", stored.Description)
				assert.Equal(t, cdbm.Labels{"location": "inventory"}, stored.Labels)
				assert.True(t, stored.Updated.After(before.Updated))
			},
		},
		{
			name: "API write after inventory read survives deletion",
			run: func(t *testing.T) {
				ctx := context.Background()
				dbSession := testExpectedRackInitDB(t)
				defer dbSession.Close()
				testExpectedRackSetupSchema(t, dbSession)
				ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{"test-provider-org"}, []string{"FORGE_PROVIDER_ADMIN"})
				ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-provider-org", ipu)
				erDAO := cdbm.NewExpectedRackDAO(dbSession)

				site := cwu.TestBuildSite(t, dbSession, ip, "concurrent-delete-site", cdbm.SiteStatusRegistered, nil, ipu)
				row, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
					ExpectedRackID: uuid.New(),
					SiteID:         site.ID,
					RackID:         "concurrent-delete",
					RackProfileID:  "profile-A",
					Name:           "original-name",
					Description:    "original-description",
					CreatedBy:      ipu.ID,
				})
				require.NoError(t, err)
				companion, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
					ExpectedRackID: uuid.New(),
					SiteID:         site.ID,
					RackID:         "unchanged-delete",
					RackProfileID:  "profile-A",
					CreatedBy:      ipu.ID,
				})
				require.NoError(t, err)
				cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedRack)(nil))
				beforeRead, err := erDAO.Get(ctx, nil, row.ID, nil, false)
				require.NoError(t, err)
				require.False(t, site.IsTimeWithinStaleInventoryThreshold(beforeRead.Updated))

				// Commit the API edit immediately before `DELETE` executes. An unlocked
				// version recheck followed by an unconditional delete would lose it.
				var newer *cdbm.ExpectedRack
				var writeErr error
				hook := &testExpectedRackBeforeDeleteHook{rowID: row.ID, beforeDelete: func() {
					newer, writeErr = erDAO.Update(ctx, nil, cdbm.ExpectedRackUpdateInput{
						ExpectedRackID: row.ID,
						Name:           cutil.GetPtr("API-name"),
						Description:    cutil.GetPtr("API-description"),
					})
				}}
				dbSession.DB.AddQueryHook(hook)
				inventory := &corev1.ExpectedRackInventory{
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				}
				mer := ManageExpectedRack{dbSession: dbSession}
				err = mer.UpdateExpectedRacksInDB(ctx, site.ID, inventory)
				require.NoError(t, err)
				require.True(t, hook.fired)
				require.NoError(t, hook.err)
				require.NoError(t, writeErr)
				require.True(t, newer.Updated.After(beforeRead.Updated))
				stored, err := erDAO.Get(ctx, nil, row.ID, nil, false)
				require.NoError(t, err)
				assert.Equal(t, newer, stored, "both API values and Updated must survive")
				_, err = erDAO.Get(ctx, nil, companion.ID, nil, false)
				require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			},
		},
		{
			name: "delete failure allows other deletions and retries on later inventory",
			run: func(t *testing.T) {
				ctx := context.Background()
				dbSession := testExpectedRackInitDB(t)
				defer dbSession.Close()
				testExpectedRackSetupSchema(t, dbSession)
				ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{"test-provider-org"}, []string{"FORGE_PROVIDER_ADMIN"})
				ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-provider-org", ipu)
				erDAO := cdbm.NewExpectedRackDAO(dbSession)

				site := cwu.TestBuildSite(t, dbSession, ip, "failed-delete-site", cdbm.SiteStatusRegistered, nil, ipu)
				row, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
					ExpectedRackID: uuid.New(),
					SiteID:         site.ID,
					RackID:         "failed-delete",
					RackProfileID:  "profile-A",
					CreatedBy:      ipu.ID,
				})
				require.NoError(t, err)
				companion, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
					ExpectedRackID: uuid.New(),
					SiteID:         site.ID,
					RackID:         "continue-delete",
					RackProfileID:  "profile-A",
					CreatedBy:      ipu.ID,
				})
				require.NoError(t, err)
				cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedRack)(nil))
				// `GetAll` orders by `created`, so process the failing row first.
				_, err = dbSession.DB.NewUpdate().Model((*cdbm.ExpectedRack)(nil)).
					Set("created = ?", companion.Created.Add(-time.Hour)).Where("id = ?", row.ID).Exec(ctx)
				require.NoError(t, err)
				rows, _, err := erDAO.GetAll(ctx, nil, cdbm.ExpectedRackFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{}, nil)
				require.NoError(t, err)
				require.Len(t, rows, 2)
				require.Equal(t, row.ID, rows[0].ID)
				require.Equal(t, companion.ID, rows[1].ID)

				_, err = dbSession.DB.ExecContext(ctx, `CREATE OR REPLACE FUNCTION fail_expected_rack_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected database failure'; END $$`)
				require.NoError(t, err)
				defer func() {
					_, cleanupErr := dbSession.DB.ExecContext(ctx, "DROP FUNCTION fail_expected_rack_delete() CASCADE")
					require.NoError(t, cleanupErr)
				}()
				_, err = dbSession.DB.ExecContext(ctx, fmt.Sprintf("CREATE TRIGGER fail_delete BEFORE DELETE ON expected_rack FOR EACH ROW WHEN (OLD.id = '%s') EXECUTE FUNCTION fail_expected_rack_delete()", row.ID))
				require.NoError(t, err)
				deleted, err := erDAO.DeleteIfUnchanged(ctx, nil, row.ID, rows[0].Updated)
				require.False(t, deleted)
				require.ErrorContains(t, err, "injected database failure")
				hook := &testExpectedRackBeforeDeleteHook{rowID: row.ID}
				dbSession.DB.AddQueryHook(hook)
				inventory := &corev1.ExpectedRackInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}
				mer := ManageExpectedRack{dbSession: dbSession}
				require.NoError(t, mer.UpdateExpectedRacksInDB(ctx, site.ID, inventory))
				require.True(t, hook.fired)
				require.ErrorContains(t, hook.err, "injected database failure")
				stored, err := erDAO.Get(ctx, nil, row.ID, nil, false)
				require.NoError(t, err)
				assert.Equal(t, &rows[0], stored)
				_, err = erDAO.Get(ctx, nil, companion.ID, nil, false)
				require.ErrorIs(t, err, cdb.ErrDoesNotExist)

				// A later inventory retries the row after removing the injected delete failure.
				_, err = dbSession.DB.ExecContext(ctx, "DROP TRIGGER fail_delete ON expected_rack")
				require.NoError(t, err)
				require.NoError(t, mer.UpdateExpectedRacksInDB(ctx, site.ID, inventory))
				_, err = erDAO.Get(ctx, nil, row.ID, nil, false)
				require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, tc.run)
	}

	ctx := context.Background()

	dbSession := testExpectedRackInitDB(t)
	defer dbSession.Close()

	testExpectedRackSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	// Build Sites
	st := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	st2 := cwu.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)
	st3 := cwu.TestBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, nil, ipu)

	// Build ExpectedRack inventory that is paginated
	// Generate data for 14 ExpectedRacks reported from Site Agent while Cloud has 18 ExpectedRacks
	pagedExpectedRacks := []*cdbm.ExpectedRack{}
	pagedInvIds := []string{}

	erDAO := cdbm.NewExpectedRackDAO(dbSession)
	for i := 0; i < 18; i++ {
		erID := uuid.New()
		// Add labels to some racks to test label handling
		var labels map[string]string
		if i%5 == 0 {
			labels = map[string]string{
				"region": fmt.Sprintf("region-%d", i/5),
				"row":    fmt.Sprintf("row-%d", i),
			}
		}
		er, cerr := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
			ExpectedRackID: erID,
			SiteID:         st.ID,
			RackID:         fmt.Sprintf("rack-%02d", i),
			RackProfileID:  fmt.Sprintf("profile-%d", i),
			RackGroupID:    new(fmt.Sprintf("group-%d", i)),
			Name:           fmt.Sprintf("Rack %d", i),
			Description:    fmt.Sprintf("Rack %d description", i),
			Labels:         labels,
			CreatedBy:      ipu.ID,
		})
		assert.NoError(t, cerr)

		// Update creation and update timestamp to be earlier than inventory processing interval so
		// reconciliation considers them outside the race-condition window.
		_, uerr := dbSession.DB.Exec("UPDATE expected_rack SET created = ?, updated = ? WHERE id = ?",
			time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)),
			time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)),
			er.ID.String())
		assert.NoError(t, uerr)

		pagedExpectedRacks = append(pagedExpectedRacks, er)
		pagedInvIds = append(pagedInvIds, er.RackID)
	}

	pageUpdates := [2][]*cdbm.ExpectedRack{}
	pagedCtrlExpectedRacks := []*corev1.ExpectedRack{}

	for i := 0; i < 14; i++ {
		ctrlExpectedRack := &corev1.ExpectedRack{
			RackId:        &corev1.RackId{Id: pagedExpectedRacks[i].RackID},
			RackProfileId: &corev1.RackProfileId{Id: pagedExpectedRacks[i].RackProfileID},
			RackGroupId:   &corev1.RackGroupId{Id: *pagedExpectedRacks[i].RackGroupID},
			Metadata: &corev1.Metadata{
				Name:        pagedExpectedRacks[i].Name,
				Description: pagedExpectedRacks[i].Description,
			},
		}

		// Echo back labels for current racks
		if i%5 == 0 {
			ctrlExpectedRack.Metadata.Labels = []*corev1.Label{
				{Key: "region", Value: cutil.GetPtr(fmt.Sprintf("region-%d", i/5))},
				{Key: "row", Value: cutil.GetPtr(fmt.Sprintf("row-%d", i))},
			}
		}

		// Have entries that need updates: change RackProfileID/Name/Description
		if i%3 == 0 {
			if i < 10 {
				ctrlExpectedRack.RackProfileId = &corev1.RackProfileId{Id: fmt.Sprintf("profile-updated-%d", i)} // Changed RackProfileID
				ctrlExpectedRack.RackGroupId = &corev1.RackGroupId{Id: fmt.Sprintf("group-updated-%d", i)}
				ctrlExpectedRack.Metadata.Name = fmt.Sprintf("Updated Rack %d", i)
				ctrlExpectedRack.Metadata.Description = fmt.Sprintf("Updated Rack %d description", i)
				pageUpdates[i/8] = append(pageUpdates[i/8], pagedExpectedRacks[i])
			}
		}

		// Test label updates
		switch i {
		case 1:
			// Add labels to a rack that didn't have them before
			ctrlExpectedRack.Metadata.Labels = []*corev1.Label{
				{Key: "new-label", Value: cutil.GetPtr("new-value")},
			}
			pageUpdates[i/8] = append(pageUpdates[i/8], pagedExpectedRacks[i])
		case 5:
			// Modify existing labels
			ctrlExpectedRack.Metadata.Labels = []*corev1.Label{
				{Key: "region", Value: cutil.GetPtr(fmt.Sprintf("region-updated-%d", i/5))},
				{Key: "row", Value: cutil.GetPtr(fmt.Sprintf("row-%d", i))},
			}
			pageUpdates[i/8] = append(pageUpdates[i/8], pagedExpectedRacks[i])
		case 10:
			// Core omits the repeated field when all labels have been removed.
			ctrlExpectedRack.Metadata.Labels = nil
			pageUpdates[i/8] = append(pageUpdates[i/8], pagedExpectedRacks[i])
		}

		pagedCtrlExpectedRacks = append(pagedCtrlExpectedRacks, ctrlExpectedRack)
	}

	racksMissingFromInventory := pagedExpectedRacks[14:18]

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	type fields struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
		env            *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx                   context.Context
		siteID                uuid.UUID
		expectedRackInventory *corev1.ExpectedRackInventory
	}

	tests := []struct {
		name                  string
		fields                fields
		args                  args
		expectedRacksToUpdate []*cdbm.ExpectedRack
		expectedRacksToDelete []*cdbm.ExpectedRack
		wantRackIDs           []string // nil skips the exact-set check
		expectedRacksToRetain []*cdbm.ExpectedRack
		wantErr               bool
	}{
		{
			name: "test ExpectedRack inventory processing error, nil inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:                   ctx,
				siteID:                st.ID,
				expectedRackInventory: nil,
			},
			wantErr: true,
		},
		{
			name: "test ExpectedRack inventory processing error, non-existent Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				expectedRackInventory: &corev1.ExpectedRackInventory{
					ExpectedRacks: []*corev1.ExpectedRack{},
				},
			},
			wantErr: true,
		},
		{
			name: "test ExpectedRack inventory processing, failed inventory status",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedRackInventory: &corev1.ExpectedRackInventory{
					ExpectedRacks:   []*corev1.ExpectedRack{},
					Timestamp:       timestamppb.Now(),
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
				},
			},
			wantErr: false,
		},
		{
			name: "test paged ExpectedRack inventory processing, empty inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				expectedRackInventory: &corev1.ExpectedRackInventory{
					ExpectedRacks:   []*corev1.ExpectedRack{},
					Timestamp:       timestamppb.Now(),
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  0,
						PageSize:    25,
						TotalItems:  0,
						ItemIds:     []string{},
					},
				},
			},
		},
		{
			name: "test paged ExpectedRack inventory processing, first page (mid-stream, no deletions yet)",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedRackInventory: &corev1.ExpectedRackInventory{
					ExpectedRacks:   pagedCtrlExpectedRacks[0:8],
					Timestamp:       timestamppb.Now(),
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  2,
						PageSize:    8,
						TotalItems:  14,
					},
				},
			},
			expectedRacksToUpdate: pageUpdates[0],
			expectedRacksToDelete: []*cdbm.ExpectedRack{},
			expectedRacksToRetain: racksMissingFromInventory,
		},
		{
			name: "test paged ExpectedRack inventory processing, last page (deletions allowed)",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedRackInventory: &corev1.ExpectedRackInventory{
					ExpectedRacks:   pagedCtrlExpectedRacks[8:14],
					Timestamp:       timestamppb.Now(),
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 2,
						TotalPages:  2,
						PageSize:    8,
						TotalItems:  14,
						ItemIds:     pagedInvIds[0:14],
					},
				},
			},
			expectedRacksToUpdate: pageUpdates[1],
			expectedRacksToDelete: racksMissingFromInventory,
		},
		{
			name: "test non-paged ExpectedRack inventory processing creates new entries (with nil + empty-id entries ignored, nil Metadata default)",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				expectedRackInventory: &corev1.ExpectedRackInventory{
					ExpectedRacks: []*corev1.ExpectedRack{
						// Valid new rack with metadata + labels
						{
							RackId:        &corev1.RackId{Id: "rack-new-1"},
							RackProfileId: &corev1.RackProfileId{Id: "profile-A"},
							Metadata: &corev1.Metadata{
								Name:        "Rack New 1",
								Description: "freshly reported",
								Labels: []*corev1.Label{
									{Key: "environment", Value: cutil.GetPtr("test")},
									{Key: "datacenter", Value: cutil.GetPtr("dc1")},
								},
							},
						},
						// Valid new rack with nil Metadata: Name/Description/Labels default to empty
						{
							RackId:        &corev1.RackId{Id: "rack-new-nil-meta"},
							RackProfileId: &corev1.RackProfileId{Id: "profile-B"},
							Metadata:      nil,
						},
						// Nil entry: should be ignored
						nil,
						// Entry with nil RackId: should be ignored
						{
							RackId:        nil,
							RackProfileId: &corev1.RackProfileId{Id: "profile-skip"},
						},
						// Entry with empty RackId.Id: should be ignored
						{
							RackId:        &corev1.RackId{Id: ""},
							RackProfileId: &corev1.RackProfileId{Id: "profile-skip"},
						},
					},
					Timestamp:       timestamppb.Now(),
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			expectedRacksToUpdate: []*cdbm.ExpectedRack{},
			expectedRacksToDelete: []*cdbm.ExpectedRack{},
			wantRackIDs:           []string{"rack-new-1", "rack-new-nil-meta"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mer := ManageExpectedRack{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
			}

			err := mer.UpdateExpectedRacksInDB(tt.args.ctx, tt.args.siteID, tt.args.expectedRackInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			// Verify state by fetching all racks for the site.
			erDAO := cdbm.NewExpectedRackDAO(dbSession)
			filterInput := cdbm.ExpectedRackFilterInput{SiteIDs: []uuid.UUID{tt.args.siteID}}
			allRacks, _, gerr := erDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			assert.NoError(t, gerr)

			// Map by RackID (operator-supplied identifier, the reconciliation key)
			racksByRackID := map[string]*cdbm.ExpectedRack{}
			for i := range allRacks {
				racksByRackID[allRacks[i].RackID] = &allRacks[i]
			}
			// Exact membership also rejects rows from invalid inventory entries.
			if tt.wantRackIDs != nil {
				actualRackIDs := make([]string, 0, len(allRacks))
				for _, rack := range allRacks {
					actualRackIDs = append(actualRackIDs, rack.RackID)
				}
				assert.ElementsMatch(t, tt.wantRackIDs, actualRackIDs)
			}

			for _, er := range tt.expectedRacksToUpdate {
				updated := racksByRackID[er.RackID]
				require.NotNil(t, updated, fmt.Sprintf("ExpectedRack %v should exist", er.RackID))
				// Find the corresponding controller rack
				var ctrlER *corev1.ExpectedRack
				for _, cer := range tt.args.expectedRackInventory.ExpectedRacks {
					if cer == nil || cer.RackId == nil {
						continue
					}
					if cer.RackId.Id == er.RackID {
						ctrlER = cer
						break
					}
				}
				require.NotNil(t, ctrlER, "ExpectedRack %v must be reported on this page", er.RackID)
				assert.Equal(t, new(ctrlER.GetRackGroupId().GetId()), updated.RackGroupID)
				assert.Equal(t, ctrlER.RackProfileId.Id, updated.RackProfileID,
					fmt.Sprintf("ExpectedRack %v RackProfileID should have been updated", er.RackID))
				reportedName := ""
				reportedDescription := ""
				if ctrlER.Metadata != nil {
					reportedName = ctrlER.Metadata.Name
					reportedDescription = ctrlER.Metadata.Description
				}
				assert.Equal(t, reportedName, updated.Name,
					fmt.Sprintf("ExpectedRack %v Name should match", er.RackID))
				assert.Equal(t, reportedDescription, updated.Description,
					fmt.Sprintf("ExpectedRack %v Description should match", er.RackID))

				var expectedLabels cdbm.Labels
				expectedLabels.FromProto(ctrlER.Metadata.GetLabels())
				if len(expectedLabels) == 0 && len(updated.Labels) == 0 {
					// Both effectively empty, which is correct
				} else {
					assert.Equal(t, expectedLabels, updated.Labels,
						fmt.Sprintf("ExpectedRack %v labels should match", er.RackID))
				}
			}

			// Verify deletions
			for _, er := range tt.expectedRacksToDelete {
				deleted := racksByRackID[er.RackID]
				assert.Nil(t, deleted, fmt.Sprintf("ExpectedRack %v should have been deleted", er.RackID))
			}
			for _, er := range tt.expectedRacksToRetain {
				assert.Contains(t, racksByRackID, er.RackID, "missing racks must remain until the final inventory page")
			}

			// Every valid reported rack must persist with its reported profile and metadata.
			for _, cer := range tt.args.expectedRackInventory.ExpectedRacks {
				if cer == nil || cer.RackId == nil || cer.RackId.Id == "" {
					continue
				}
				persisted := racksByRackID[cer.RackId.Id]
				require.NotNil(t, persisted, "ExpectedRack %v should exist", cer.RackId.Id)
				assert.Equal(t, cer.GetRackProfileId().GetId(), persisted.RackProfileID,
					"ExpectedRack %v RackProfileID should match reported inventory", cer.RackId.Id)
				assert.Equal(t, cer.GetMetadata().GetName(), persisted.Name,
					"ExpectedRack %v Name should match reported inventory", cer.RackId.Id)
				assert.Equal(t, cer.GetMetadata().GetDescription(), persisted.Description,
					"ExpectedRack %v Description should match reported inventory", cer.RackId.Id)

				var expectedLabels cdbm.Labels
				expectedLabels.FromProto(cer.Metadata.GetLabels())
				if len(expectedLabels) == 0 {
					assert.Empty(t, persisted.Labels)
				} else {
					assert.Equal(t, expectedLabels, persisted.Labels,
						"ExpectedRack %v labels should match reported inventory", cer.RackId.Id)
				}
			}
		})
	}
}

func testExpectedRackInventoryNoChange(t *testing.T) {
	// Core can omit labels for an unlabeled rack; the DB stores an empty map.
	cases := []struct {
		name           string
		storedLabels   cdbm.Labels
		reportedLabels []*corev1.Label
	}{
		{
			name:           "with labels",
			storedLabels:   cdbm.Labels{"foo": "bar"},
			reportedLabels: []*corev1.Label{{Key: "foo", Value: cutil.GetPtr("bar")}},
		},
		{
			name:           "without labels",
			storedLabels:   cdbm.Labels{},
			reportedLabels: nil,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()

			dbSession := testExpectedRackInitDB(t)
			defer dbSession.Close()

			testExpectedRackSetupSchema(t, dbSession)

			ipOrg := "test-provider-org"
			ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

			ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
			ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
			st := cwu.TestBuildSite(t, dbSession, ip, "test-site-nochange", cdbm.SiteStatusRegistered, nil, ipu)

			erDAO := cdbm.NewExpectedRackDAO(dbSession)
			er, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
				ExpectedRackID: uuid.New(),
				SiteID:         st.ID,
				RackID:         "rack-nc-1",
				RackProfileID:  "profile-nc",
				Name:           "Rack NC",
				Description:    "no change",
				Labels:         tc.storedLabels,
				CreatedBy:      ipu.ID,
			})
			require.NoError(t, err)

			// Age the row past the inventory threshold so a redundant write would
			// advance `Updated` instead of being skipped by the freshness check.
			cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedRack)(nil))
			before, err := erDAO.Get(ctx, nil, er.ID, nil, false)
			require.NoError(t, err)
			require.False(t, st.IsTimeWithinStaleInventoryThreshold(before.Updated))
			require.Equal(t, tc.storedLabels, before.Labels)

			mer := ManageExpectedRack{dbSession: dbSession}
			inventory := &corev1.ExpectedRackInventory{
				ExpectedRacks: []*corev1.ExpectedRack{
					{
						RackId:        &corev1.RackId{Id: er.RackID},
						RackProfileId: &corev1.RackProfileId{Id: er.RackProfileID},
						Metadata: &corev1.Metadata{
							Name:        er.Name,
							Description: er.Description,
							Labels:      tc.reportedLabels,
						},
					},
				},
				Timestamp:       timestamppb.Now(),
				InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			}

			err = mer.UpdateExpectedRacksInDB(ctx, st.ID, inventory)
			require.NoError(t, err)

			got, err := erDAO.Get(ctx, nil, er.ID, nil, false)
			require.NoError(t, err)
			assert.Equal(t, before.Updated, got.Updated,
				"row should not have been updated when nothing changed")
		})
	}
}

func testExpectedRackInventoryRecentWrite(t *testing.T) {
	ctx := context.Background()

	dbSession := testExpectedRackInitDB(t)
	defer dbSession.Close()

	testExpectedRackSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	st := cwu.TestBuildSite(t, dbSession, ip, "test-site-race", cdbm.SiteStatusRegistered, nil, ipu)

	// Create an ExpectedRack: newly-created record will have a timestamp within the race-condition window
	erDAO := cdbm.NewExpectedRackDAO(dbSession)
	recentER, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		SiteID:         st.ID,
		RackID:         "rack-recent",
		RackProfileID:  "profile-recent",
		Name:           "Recent",
		CreatedBy:      ipu.ID,
	})
	assert.NoError(t, err)

	tSiteClientPool := testTemporalSiteClientPool(t)

	mer := ManageExpectedRack{
		dbSession:      dbSession,
		siteClientPool: tSiteClientPool,
	}

	// Send inventory without this rack - it should NOT be deleted due to race condition
	inventory := &corev1.ExpectedRackInventory{
		ExpectedRacks:   []*corev1.ExpectedRack{},
		Timestamp:       timestamppb.Now(),
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
	}

	err = mer.UpdateExpectedRacksInDB(ctx, st.ID, inventory)
	assert.NoError(t, err)

	// Verify the rack was NOT deleted
	filterInput := cdbm.ExpectedRackFilterInput{SiteIDs: []uuid.UUID{st.ID}}
	allRacks, _, gerr := erDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	assert.NoError(t, gerr)

	found := false
	for _, er := range allRacks {
		if er.ID == recentER.ID {
			found = true
			break
		}
	}
	assert.True(t, found, "Recently created ExpectedRack should NOT be deleted due to race condition")
}

func testExpectedRackInventoryRecentReportedWrite(t *testing.T) {
	ctx := context.Background()
	dbSession := testExpectedRackInitDB(t)
	defer dbSession.Close()
	testExpectedRackSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{"FORGE_PROVIDER_ADMIN"})
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "recent-reported-site", cdbm.SiteStatusRegistered, nil, ipu)
	erDAO := cdbm.NewExpectedRackDAO(dbSession)
	companion, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		SiteID:         site.ID,
		RackID:         "aged-reported-rack",
		RackProfileID:  "companion-profile",
		Name:           "saved companion",
		Description:    "saved companion description",
		CreatedBy:      ipu.ID,
	})
	require.NoError(t, err)
	cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedRack)(nil))
	beforeCompanion, err := erDAO.Get(ctx, nil, companion.ID, nil, false)
	require.NoError(t, err)
	require.False(t, site.IsTimeWithinStaleInventoryThreshold(beforeCompanion.Updated))
	row, err := erDAO.Create(ctx, nil, cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		SiteID:         site.ID,
		RackID:         "recent-reported-rack",
		RackProfileID:  "saved-profile",
		RackGroupID:    cutil.GetPtr("saved-group"),
		Name:           "saved name",
		Description:    "saved description",
		Labels:         map[string]string{"location": "saved"},
		CreatedBy:      ipu.ID,
	})
	require.NoError(t, err)
	beforeInventory, err := erDAO.Get(ctx, nil, row.ID, nil, false)
	require.NoError(t, err)

	inventory := &corev1.ExpectedRackInventory{
		ExpectedRacks: []*corev1.ExpectedRack{{
			RackId:        &corev1.RackId{Id: row.RackID},
			RackProfileId: &corev1.RackProfileId{Id: "inventory-profile"},
			RackGroupId:   &corev1.RackGroupId{Id: "inventory-group"},
			Metadata: &corev1.Metadata{
				Name:        "inventory name",
				Description: "inventory description",
				Labels:      []*corev1.Label{{Key: "location", Value: cutil.GetPtr("inventory")}},
			},
		}, {
			RackId:        &corev1.RackId{Id: companion.RackID},
			RackProfileId: &corev1.RackProfileId{Id: companion.RackProfileID},
			Metadata:      &corev1.Metadata{Name: "inventory companion", Description: "inventory companion description"},
		}},
		Timestamp:       timestamppb.Now(),
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
	}
	mer := ManageExpectedRack{dbSession: dbSession}
	require.True(t, site.IsTimeWithinStaleInventoryThreshold(beforeInventory.Updated))
	require.NoError(t, mer.UpdateExpectedRacksInDB(ctx, site.ID, inventory))

	stored, err := erDAO.Get(ctx, nil, row.ID, nil, false)
	require.NoError(t, err)
	assert.Equal(t, beforeInventory, stored, "recent values and Updated must survive conflicting inventory")
	stored, err = erDAO.Get(ctx, nil, companion.ID, nil, false)
	require.NoError(t, err)
	assert.Equal(t, "inventory companion", stored.Name)
	assert.Equal(t, "inventory companion description", stored.Description)
	assert.True(t, stored.Updated.After(beforeCompanion.Updated), "skipping a recent rack must still allow later updates")
}

func TestNewManageExpectedRack(t *testing.T) {
	type args struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
	}

	dbSession := &cdb.Session{}
	keyPath, certPath := config.SetupTestCerts(t)
	defer os.Remove(keyPath)
	defer os.Remove(certPath)

	cfg := config.NewConfig()
	cfg.SetTemporalCertPath(certPath)
	cfg.SetTemporalKeyPath(keyPath)
	cfg.SetTemporalCaPath(certPath)
	tcfg, err := cfg.GetTemporalConfig()
	assert.NoError(t, err)
	scp := sc.NewClientPool(tcfg)

	tests := []struct {
		name string
		args args
		want ManageExpectedRack
	}{
		{
			name: "test new ManageExpectedRack instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageExpectedRack{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewManageExpectedRack(tt.args.dbSession, tt.args.siteClientPool)
			assert.Equal(t, tt.want.dbSession, got.dbSession, "dbSession should match")
			assert.Equal(t, tt.want.siteClientPool, got.siteClientPool, "siteClientPool should match")
		})
	}
}
