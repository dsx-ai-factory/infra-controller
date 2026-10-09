// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedpowershelf

import (
	"context"
	"fmt"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"go.temporal.io/sdk/testsuite"
	"google.golang.org/protobuf/proto"
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
	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// testTemporalSiteClientPool Building site client pool
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

func testExpectedPowerShelfInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testExpectedPowerShelfSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create ExpectedPowerShelf table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.ExpectedPowerShelf)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
}

type testExpectedPowerShelfReconcileContextKey struct{}

type testExpectedPowerShelfAfterReadHook struct {
	afterRead    func()
	beforeCommit func()
	rowID        uuid.UUID
}

func (h *testExpectedPowerShelfAfterReadHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if ctx.Value(testExpectedPowerShelfReconcileContextKey{}) == h && event.Operation() == "COMMIT" && h.beforeCommit != nil {
		h.beforeCommit()
	}
	return ctx
}

func (h *testExpectedPowerShelfAfterReadHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
	if h.rowID != uuid.Nil && (ctx.Value(testExpectedPowerShelfReconcileContextKey{}) != h || !strings.Contains(event.Query, h.rowID.String())) {
		return
	}
	if h.afterRead == nil || event.Err != nil || event.Operation() != "SELECT" ||
		strings.HasPrefix(event.Query, "SELECT count(") || !strings.Contains(event.Query, `FROM "expected_power_shelf"`) {
		return
	}
	afterRead := h.afterRead
	h.afterRead = nil
	afterRead()
}

func TestManageExpectedPowerShelf_UpdateExpectedPowerShelvesInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := testExpectedPowerShelfInitDB(t)
	defer dbSession.Close()

	testExpectedPowerShelfSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	// Build Sites
	st := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	st2 := cwu.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)
	st3 := cwu.TestBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, nil, ipu)

	// Build ExpectedPowerShelf inventory that is paginated
	// Generate data for 34 ExpectedPowerShelves reported from Site Agent while Cloud has 38 ExpectedPowerShelves
	pagedExpectedPowerShelves := []*cdbm.ExpectedPowerShelf{}
	pagedInvIds := []string{}

	epsDAO := cdbm.NewExpectedPowerShelfDAO(dbSession)
	for i := 0; i < 38; i++ {
		epsID := uuid.New()
		// Add labels to some power shelves to test label handling
		var labels map[string]string
		if i%5 == 0 {
			labels = map[string]string{
				"rack":     fmt.Sprintf("rack-%d", i/5),
				"position": fmt.Sprintf("pos-%d", i),
			}
		}
		// Set BmcIpAddress for every 3rd entry
		var bmcIpAddress *string
		if i%3 == 0 {
			ip := fmt.Sprintf("10.0.0.%d", i)
			bmcIpAddress = &ip
		}
		eps, cerr := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
			ExpectedPowerShelfID: epsID,
			SiteID:               st.ID,
			BmcMacAddress:        fmt.Sprintf("00:11:22:33:44:%02d", i),
			ShelfSerialNumber:    fmt.Sprintf("SHELF-SN-%d", i),
			BmcIpAddress:         bmcIpAddress,
			Labels:               labels,
			CreatedBy:            ipu.ID,
		})
		assert.NoError(t, cerr)

		// Update creation and update timestamp to be earlier than inventory processing interval
		_, uerr := dbSession.DB.Exec("UPDATE expected_power_shelf SET created = ?, updated = ? WHERE id = ?",
			time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval*2)),
			time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval*2)),
			eps.ID.String())
		assert.NoError(t, uerr)

		pagedExpectedPowerShelves = append(pagedExpectedPowerShelves, eps)
		pagedInvIds = append(pagedInvIds, eps.ID.String())
	}

	expectedPowerShelvesToUpdate := []*cdbm.ExpectedPowerShelf{}
	pagedCtrlExpectedPowerShelves := []*corev1.ExpectedPowerShelf{}

	for i := 0; i < 34; i++ {
		// Convert DB BmcIpAddress (*string) to proto BmcIpAddress (string)
		protoBmcIpAddress := ""
		if pagedExpectedPowerShelves[i].BmcIpAddress != nil {
			protoBmcIpAddress = *pagedExpectedPowerShelves[i].BmcIpAddress
		}

		ctrlExpectedPowerShelf := &corev1.ExpectedPowerShelf{
			ExpectedPowerShelfId: &corev1.UUID{Value: pagedExpectedPowerShelves[i].ID.String()},
			BmcMacAddress:        pagedExpectedPowerShelves[i].BmcMacAddress,
			ShelfSerialNumber:    pagedExpectedPowerShelves[i].ShelfSerialNumber,
			BmcIpAddress:         protoBmcIpAddress,
		}

		// Add labels to controller expected power shelves
		if i%5 == 0 {
			ctrlExpectedPowerShelf.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "rack", Value: cutil.GetPtr(fmt.Sprintf("rack-%d", i/5))},
					{Key: "position", Value: cutil.GetPtr(fmt.Sprintf("pos-%d", i))},
				},
			}
		}

		// Have entries that need updates
		if i%3 == 0 {
			if i < 20 {
				ctrlExpectedPowerShelf.BmcMacAddress = fmt.Sprintf("00:11:22:33:55:%02d", i) // Changed MAC
				expectedPowerShelvesToUpdate = append(expectedPowerShelvesToUpdate, pagedExpectedPowerShelves[i])
			}
		}

		// Test BmcIpAddress updates: change BmcIpAddress for some entries
		if i == 2 {
			ctrlExpectedPowerShelf.BmcIpAddress = "192.168.1.100" // Add IP to entry that didn't have one
			expectedPowerShelvesToUpdate = append(expectedPowerShelvesToUpdate, pagedExpectedPowerShelves[i])
		}

		// Test label updates: add/modify labels for some power shelves
		if i == 1 {
			// Add labels to a power shelf that didn't have them before
			ctrlExpectedPowerShelf.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "new-label", Value: cutil.GetPtr("new-value")},
				},
			}
			expectedPowerShelvesToUpdate = append(expectedPowerShelvesToUpdate, pagedExpectedPowerShelves[i])
		} else if i == 5 {
			// Modify existing labels
			ctrlExpectedPowerShelf.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "rack", Value: cutil.GetPtr(fmt.Sprintf("rack-updated-%d", i/5))},
					{Key: "position", Value: cutil.GetPtr(fmt.Sprintf("pos-%d", i))},
					{Key: "status", Value: cutil.GetPtr("active")},
				},
			}
			expectedPowerShelvesToUpdate = append(expectedPowerShelvesToUpdate, pagedExpectedPowerShelves[i])
		} else if i == 10 {
			// Remove labels (set to empty labels array)
			ctrlExpectedPowerShelf.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{},
			}
			expectedPowerShelvesToUpdate = append(expectedPowerShelvesToUpdate, pagedExpectedPowerShelves[i])
		} else if i == 15 {
			// Remove labels (set metadata to nil)
			ctrlExpectedPowerShelf.Metadata = nil
			expectedPowerShelvesToUpdate = append(expectedPowerShelvesToUpdate, pagedExpectedPowerShelves[i])
		}

		pagedCtrlExpectedPowerShelves = append(pagedCtrlExpectedPowerShelves, ctrlExpectedPowerShelf)
	}

	expectedPowerShelvesToDelete := pagedExpectedPowerShelves[34:38]

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
		ctx                         context.Context
		siteID                      uuid.UUID
		expectedPowerShelfInventory *corev1.ExpectedPowerShelfInventory
	}

	tests := []struct {
		name                         string
		fields                       fields
		args                         args
		expectedPowerShelvesToUpdate []*cdbm.ExpectedPowerShelf
		expectedPowerShelvesToDelete []*cdbm.ExpectedPowerShelf
		unchangedID                  uuid.UUID
		wantErr                      bool
	}{
		{
			name: "test ExpectedPowerShelf inventory processing error, nil inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:                         ctx,
				siteID:                      st.ID,
				expectedPowerShelfInventory: nil,
			},
			wantErr: true,
		},
		{
			name: "test ExpectedPowerShelf inventory processing error, non-existent Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				expectedPowerShelfInventory: &corev1.ExpectedPowerShelfInventory{
					ExpectedPowerShelves: []*corev1.ExpectedPowerShelf{},
				},
			},
			wantErr: true,
		},
		{
			name: "test ExpectedPowerShelf inventory processing, failed inventory status",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedPowerShelfInventory: &corev1.ExpectedPowerShelfInventory{
					ExpectedPowerShelves: []*corev1.ExpectedPowerShelf{},
					Timestamp:            timestamppb.Now(),
					InventoryStatus:      corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
				},
			},
			wantErr: false,
		},
		{
			name: "test paged ExpectedPowerShelf inventory processing, empty inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				expectedPowerShelfInventory: &corev1.ExpectedPowerShelfInventory{
					ExpectedPowerShelves: []*corev1.ExpectedPowerShelf{},
					Timestamp:            timestamppb.Now(),
					InventoryStatus:      corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
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
			name: "test paged ExpectedPowerShelf inventory processing, first page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedPowerShelfInventory: &corev1.ExpectedPowerShelfInventory{
					ExpectedPowerShelves: pagedCtrlExpectedPowerShelves[0:20],
					Timestamp:            timestamppb.Now(),
					InventoryStatus:      corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  2,
						PageSize:    20,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			expectedPowerShelvesToUpdate: expectedPowerShelvesToUpdate,
			expectedPowerShelvesToDelete: []*cdbm.ExpectedPowerShelf{},
			unchangedID:                  pagedExpectedPowerShelves[7].ID,
		},
		{
			name: "test paged ExpectedPowerShelf inventory processing, last page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedPowerShelfInventory: &corev1.ExpectedPowerShelfInventory{
					ExpectedPowerShelves: pagedCtrlExpectedPowerShelves[20:34],
					Timestamp:            timestamppb.Now(),
					InventoryStatus:      corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 2,
						TotalPages:  2,
						PageSize:    20,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			expectedPowerShelvesToUpdate: []*cdbm.ExpectedPowerShelf{},
			expectedPowerShelvesToDelete: expectedPowerShelvesToDelete,
		},
		{
			name: "test non-paged ExpectedPowerShelf inventory processing",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				expectedPowerShelfInventory: &corev1.ExpectedPowerShelfInventory{
					ExpectedPowerShelves: []*corev1.ExpectedPowerShelf{
						{
							ExpectedPowerShelfId: &corev1.UUID{Value: uuid.New().String()},
							BmcMacAddress:        "00:11:22:33:44:FF",
							ShelfSerialNumber:    "SHELF-SN-NEW-1",
							BmcIpAddress:         "10.0.0.100",
							Metadata: &corev1.Metadata{
								Labels: []*corev1.Label{
									{Key: "environment", Value: cutil.GetPtr("test")},
									{Key: "datacenter", Value: cutil.GetPtr("dc1")},
								},
							},
						},
					},
					Timestamp:       timestamppb.Now(),
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			expectedPowerShelvesToUpdate: []*cdbm.ExpectedPowerShelf{},
			expectedPowerShelvesToDelete: []*cdbm.ExpectedPowerShelf{},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mei := ManageExpectedPowerShelf{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
			}

			var beforeUnchanged *cdbm.ExpectedPowerShelf
			if tt.unchangedID != uuid.Nil {
				var err error
				beforeUnchanged, err = epsDAO.Get(ctx, nil, tt.unchangedID, nil, false)
				require.NoError(t, err)
				require.Nil(t, beforeUnchanged.Labels)
				require.False(t, st.IsTimeWithinStaleInventoryThreshold(beforeUnchanged.Updated))
			}

			err := mei.UpdateExpectedPowerShelvesInDB(tt.args.ctx, tt.args.siteID, tt.args.expectedPowerShelfInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			// Verify updates by fetching all power shelves for the site
			epsDAO := cdbm.NewExpectedPowerShelfDAO(dbSession)
			filterInput := cdbm.ExpectedPowerShelfFilterInput{SiteIDs: []uuid.UUID{tt.args.siteID}}
			allPowerShelves, _, gerr := epsDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			assert.NoError(t, gerr)

			// Build a map of power shelves by ID for easy lookup
			powerShelvesByID := map[uuid.UUID]*cdbm.ExpectedPowerShelf{}
			for i := range allPowerShelves {
				powerShelvesByID[allPowerShelves[i].ID] = &allPowerShelves[i]
			}

			if beforeUnchanged != nil {
				unchanged := powerShelvesByID[beforeUnchanged.ID]
				require.NotNil(t, unchanged)
				assert.Equal(t, beforeUnchanged.Updated, unchanged.Updated, "absent labels must not refresh Updated on the first reconciliation")
			}

			for _, eps := range tt.expectedPowerShelvesToUpdate {
				updated := powerShelvesByID[eps.ID]
				assert.NotNil(t, updated, fmt.Sprintf("ExpectedPowerShelf %v should exist", eps.ID))
				// Find the corresponding controller power shelf
				var ctrlEPS *corev1.ExpectedPowerShelf
				for _, ceps := range tt.args.expectedPowerShelfInventory.ExpectedPowerShelves {
					if ceps.ExpectedPowerShelfId.Value == eps.ID.String() {
						ctrlEPS = ceps
						break
					}
				}
				if ctrlEPS != nil {
					assert.Equal(t, ctrlEPS.BmcMacAddress, updated.BmcMacAddress,
						fmt.Sprintf("ExpectedPowerShelf %v should have been updated", eps.ID))

					// Verify BmcIpAddress is updated correctly
					if ctrlEPS.BmcIpAddress == "" {
						assert.Nil(t, updated.BmcIpAddress,
							fmt.Sprintf("ExpectedPowerShelf %v should have nil BmcIpAddress", eps.ID))
					} else {
						if assert.NotNil(t, updated.BmcIpAddress,
							fmt.Sprintf("ExpectedPowerShelf %v should have BmcIpAddress set", eps.ID)) {
							assert.Equal(t, ctrlEPS.BmcIpAddress, *updated.BmcIpAddress,
								fmt.Sprintf("ExpectedPowerShelf %v BmcIpAddress should match", eps.ID))
						}
					}

					// Verify labels are updated correctly
					var expectedLabels cdbm.Labels
					expectedLabels.FromProto(ctrlEPS.Metadata.GetLabels())
					// Both nil and empty maps should be treated as equivalent (no labels)
					if len(expectedLabels) == 0 && len(updated.Labels) == 0 {
						// Both are effectively empty, which is correct
					} else {
						assert.Equal(t, expectedLabels, updated.Labels,
							fmt.Sprintf("ExpectedPowerShelf %v labels should match", eps.ID))
					}
				}
			}

			// Verify deletions
			for _, eps := range tt.expectedPowerShelvesToDelete {
				deleted := powerShelvesByID[eps.ID]
				assert.Nil(t, deleted, fmt.Sprintf("ExpectedPowerShelf %v should have been deleted", eps.ID))
			}

			// Verify newly created power shelves have correct labels and BmcIpAddress
			for _, ceps := range tt.args.expectedPowerShelfInventory.ExpectedPowerShelves {
				epsID, perr := uuid.Parse(ceps.ExpectedPowerShelfId.Value)
				assert.NoError(t, perr)
				created := powerShelvesByID[epsID]
				if created != nil {
					var expectedLabels cdbm.Labels
					expectedLabels.FromProto(ceps.Metadata.GetLabels())
					// Both nil and empty maps should be treated as equivalent (no labels)
					if len(expectedLabels) == 0 && len(created.Labels) == 0 {
						// Both are effectively empty, which is correct
					} else {
						assert.Equal(t, expectedLabels, created.Labels,
							fmt.Sprintf("ExpectedPowerShelf %v labels should match on creation", epsID))
					}
				}
			}
		})
	}
	t.Run("API write after inventory read survives reconciliation", func(t *testing.T) {
		site := cwu.TestBuildSite(t, dbSession, ip, "concurrent-update-site", cdbm.SiteStatusRegistered, nil, ipu)
		row, err := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
			ExpectedPowerShelfID: uuid.New(),
			SiteID:               site.ID,
			BmcMacAddress:        "00:11:22:33:99:01",
			ShelfSerialNumber:    "SN-CONCURRENT",
			Name:                 cutil.GetPtr("original-name"),
			Description:          cutil.GetPtr("original-description"),
			CreatedBy:            ipu.ID,
		})
		require.NoError(t, err)
		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedPowerShelf)(nil))

		// Commit the API edit after the inventory read has cached the old row.
		var newer *cdbm.ExpectedPowerShelf
		var writeErr error
		fired := false
		hook := &testExpectedPowerShelfAfterReadHook{afterRead: func() {
			fired = true
			newer, writeErr = epsDAO.Update(ctx, nil, cdbm.ExpectedPowerShelfUpdateInput{
				ExpectedPowerShelfID: row.ID,
				Name:                 cutil.GetPtr("API-name"),
				Description:          cutil.GetPtr("API-description"),
			})
		}}
		dbSession.DB.AddQueryHook(hook)
		defer func() { hook.afterRead = nil }()
		inventory := &corev1.ExpectedPowerShelfInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			ExpectedPowerShelves: []*corev1.ExpectedPowerShelf{{
				ExpectedPowerShelfId: &corev1.UUID{Value: row.ID.String()},
				BmcMacAddress:        row.BmcMacAddress,
				ShelfSerialNumber:    row.ShelfSerialNumber,
				Metadata:             &corev1.Metadata{Name: "inventory-name", Description: "inventory-description"},
			}},
		}
		mei := ManageExpectedPowerShelf{dbSession: dbSession}
		err = mei.UpdateExpectedPowerShelvesInDB(ctx, site.ID, inventory)
		require.True(t, fired)
		require.NoError(t, writeErr)
		require.NoError(t, err)
		stored, err := epsDAO.Get(ctx, nil, row.ID, nil, false)
		require.NoError(t, err)
		assert.Equal(t, newer, stored, "both API values and Updated must survive")

		// A later inventory can still replace the supplied metadata.
		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedPowerShelf)(nil))
		err = mei.UpdateExpectedPowerShelvesInDB(ctx, site.ID, inventory)
		require.NoError(t, err)
		stored, err = epsDAO.Get(ctx, nil, row.ID, nil, false)
		require.NoError(t, err)
		assert.Equal(t, cutil.GetPtr("inventory-name"), stored.Name)
		assert.Equal(t, cutil.GetPtr("inventory-description"), stored.Description)
	})

	t.Run("row lock protects reconciliation through commit", func(t *testing.T) {
		ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
		defer cancel()
		site := cwu.TestBuildSite(t, dbSession, ip, "locked-update-site", cdbm.SiteStatusRegistered, nil, ipu)
		row, err := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
			ExpectedPowerShelfID: uuid.New(),
			SiteID:               site.ID,
			BmcMacAddress:        "00:11:22:33:99:02",
			ShelfSerialNumber:    "SN-LOCKED",
			Name:                 cutil.GetPtr("original-name"),
			Description:          cutil.GetPtr("original-description"),
			CreatedBy:            ipu.ID,
		})
		require.NoError(t, err)
		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedPowerShelf)(nil))

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
		hook := &testExpectedPowerShelfAfterReadHook{
			rowID: row.ID,
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
		inventory := &corev1.ExpectedPowerShelfInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			ExpectedPowerShelves: []*corev1.ExpectedPowerShelf{{
				ExpectedPowerShelfId: &corev1.UUID{Value: row.ID.String()},
				BmcMacAddress:        row.BmcMacAddress,
				ShelfSerialNumber:    row.ShelfSerialNumber,
				Metadata:             &corev1.Metadata{Name: "inventory-name", Description: "inventory-description"},
			}},
		}
		mei := ManageExpectedPowerShelf{dbSession: dbSession}
		var activityErr, writerErr error
		workers.Go(func() {
			activityErr = mei.UpdateExpectedPowerShelvesInDB(context.WithValue(ctx, testExpectedPowerShelfReconcileContextKey{}, hook), site.ID, inventory)
		})
		select {
		case <-readReached:
		case <-ctx.Done():
			t.Fatal("activity did not reach its row reload: ", ctx.Err())
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
				_, err = epsDAO.Update(ctx, tx, cdbm.ExpectedPowerShelfUpdateInput{
					ExpectedPowerShelfID: row.ID,
					Name:                 cutil.GetPtr("API-name"),
				})
				return err
			})
		})
		var writerPID int
		select {
		case writerPID = <-writerReady:
		case <-ctx.Done():
			t.Fatal("API writer did not start: ", ctx.Err())
		}
		assertWriterBlocked := func() {
			t.Helper()
			require.Eventually(t, func() bool {
				var blockers int
				err := dbSession.DB.NewSelect().ColumnExpr("cardinality(pg_blocking_pids(?))", writerPID).Scan(ctx, &blockers)
				return err == nil && blockers > 0
			}, 5*time.Second, 10*time.Millisecond, "API write did not wait for reconciliation")
		}
		assertWriterBlocked()
		releaseRead()
		select {
		case <-commitReached:
		case <-ctx.Done():
			t.Fatal("activity did not reach commit: ", ctx.Err())
		}
		// The writer must still wait after reconciliation has written the row.
		assertWriterBlocked()
		releaseCommit()
		workers.Wait()
		require.NoError(t, activityErr)
		require.NoError(t, writerErr)
		stored, err := epsDAO.Get(ctx, nil, row.ID, nil, false)
		require.NoError(t, err)
		assert.Equal(t, cutil.GetPtr("API-name"), stored.Name)
		assert.Equal(t, cutil.GetPtr("inventory-description"), stored.Description)
	})

	t.Run("sync supplied fields and preserve Cloud metadata", func(t *testing.T) {
		site := cwu.TestBuildSite(t, dbSession, ip, "snapshot-site", cdbm.SiteStatusRegistered, nil, ipu)
		id := uuid.New()
		mei := ManageExpectedPowerShelf{dbSession: dbSession}
		cases := []struct {
			name     string
			prepare  func(t *testing.T)
			reported *corev1.ExpectedPowerShelf
			want     cdbm.ExpectedPowerShelf
		}{
			{
				name: "create supplied Core fields",
				reported: &corev1.ExpectedPowerShelf{
					BmcIpAddress: "192.0.2.10",
					RackId:       &corev1.RackId{Id: "initial-rack"},
					Metadata: &corev1.Metadata{
						Name:        "initial-name",
						Description: "initial-description",
						Labels: []*corev1.Label{
							{Key: "model", Value: cutil.GetPtr("initial-model")},
						},
					},
				},
				want: cdbm.ExpectedPowerShelf{
					BmcIpAddress: cutil.GetPtr("192.0.2.10"),
					RackID:       cutil.GetPtr("initial-rack"),
					Name:         cutil.GetPtr("initial-name"),
					Description:  cutil.GetPtr("initial-description"),
					Labels:       cdbm.Labels{"model": "initial-model"},
				},
			},
		}
		for _, field := range []struct {
			name  string
			apply func(*corev1.ExpectedPowerShelf, *cdbm.ExpectedPowerShelf)
		}{
			{"rack", func(reported *corev1.ExpectedPowerShelf, want *cdbm.ExpectedPowerShelf) {
				reported.RackId.Id = "updated-rack"
				want.RackID = cutil.GetPtr("updated-rack")
			}},
			{"name", func(reported *corev1.ExpectedPowerShelf, want *cdbm.ExpectedPowerShelf) {
				reported.Metadata.Name = "updated-name"
				want.Name = cutil.GetPtr("updated-name")
			}},
			{"description", func(reported *corev1.ExpectedPowerShelf, want *cdbm.ExpectedPowerShelf) {
				reported.Metadata.Description = "updated-description"
				want.Description = cutil.GetPtr("updated-description")
			}},
		} {
			next := cases[len(cases)-1]
			next.name = "replace only " + field.name
			next.reported = proto.Clone(next.reported).(*corev1.ExpectedPowerShelf)
			field.apply(next.reported, &next.want)
			cases = append(cases, next)
		}

		preserved := cases[len(cases)-1]
		preserved.name = "preserve Cloud metadata when Core omits it"
		preserved.reported = &corev1.ExpectedPowerShelf{Metadata: &corev1.Metadata{Labels: []*corev1.Label{
			{Key: "model", Value: cutil.GetPtr("updated-model-label")},
		}}}
		preserved.want.Name = cutil.GetPtr("Cloud-name")
		preserved.want.Description = cutil.GetPtr("Cloud-description")
		preserved.want.RackID = cutil.GetPtr("Cloud-rack")
		preserved.want.Manufacturer = cutil.GetPtr("Cloud-manufacturer")
		preserved.want.Model = cutil.GetPtr("Cloud-model")
		preserved.want.SlotID = cutil.GetPtr(int32(3))
		preserved.want.TrayIdx = cutil.GetPtr(int32(4))
		preserved.want.HostID = cutil.GetPtr(int32(5))
		preserved.want.Labels = cdbm.Labels{"model": "updated-model-label"}
		preserved.prepare = func(t *testing.T) {
			_, err := epsDAO.Update(ctx, nil, cdbm.ExpectedPowerShelfUpdateInput{
				ExpectedPowerShelfID: id,
				Name:                 preserved.want.Name,
				Description:          preserved.want.Description,
				RackID:               preserved.want.RackID,
				Manufacturer:         preserved.want.Manufacturer,
				Model:                preserved.want.Model,
				SlotID:               preserved.want.SlotID,
				TrayIdx:              preserved.want.TrayIdx,
				HostID:               preserved.want.HostID,
			})
			require.NoError(t, err)
		}
		cases = append(cases, preserved)

		withoutLabels := preserved
		withoutLabels.name = "clear omitted labels without clearing Cloud metadata"
		withoutLabels.prepare = nil
		withoutLabels.reported = &corev1.ExpectedPowerShelf{}
		withoutLabels.want.Labels = cdbm.Labels{}
		cases = append(cases, withoutLabels)

		var created time.Time
		createdBy := site.ID
		for _, tc := range cases {
			t.Run(tc.name, func(t *testing.T) {
				if tc.prepare != nil {
					tc.prepare(t)
				}
				tc.reported.ExpectedPowerShelfId = &corev1.UUID{Value: id.String()}
				tc.reported.BmcMacAddress = "00:11:22:33:88:01"
				tc.reported.ShelfSerialNumber = "SN-SNAPSHOT"
				inventory := &corev1.ExpectedPowerShelfInventory{
					ExpectedPowerShelves: []*corev1.ExpectedPowerShelf{tc.reported},
					InventoryStatus:      corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				}
				cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedPowerShelf)(nil))
				err := mei.UpdateExpectedPowerShelvesInDB(ctx, site.ID, inventory)
				require.NoError(t, err)
				stored, err := epsDAO.Get(ctx, nil, id, nil, false)
				require.NoError(t, err)
				if created.IsZero() {
					created = stored.Created
				}
				tc.want.ID = id
				tc.want.SiteID = site.ID
				tc.want.BmcMacAddress = tc.reported.BmcMacAddress
				tc.want.ShelfSerialNumber = tc.reported.ShelfSerialNumber
				tc.want.Created = created
				tc.want.CreatedBy = createdBy
				tc.want.Updated = stored.Updated
				assert.Equal(t, tc.want, *stored)

				// Repeating an aged snapshot must not refresh `Updated`.
				cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedPowerShelf)(nil))
				beforeRepeat, err := epsDAO.Get(ctx, nil, id, nil, false)
				require.NoError(t, err)
				err = mei.UpdateExpectedPowerShelvesInDB(ctx, site.ID, inventory)
				require.NoError(t, err)
				afterRepeat, err := epsDAO.Get(ctx, nil, id, nil, false)
				require.NoError(t, err)
				assert.Equal(t, beforeRepeat.Updated, afterRepeat.Updated)

				// Subsequent snapshots must preserve the Cloud creator.
				createdBy = ipu.ID
				_, err = dbSession.DB.Exec("UPDATE expected_power_shelf SET created_by = ? WHERE id = ?", createdBy, id)
				require.NoError(t, err)
			})
		}
	})
}

func TestManageExpectedPowerShelf_UpdateExpectedPowerShelvesInDB_RaceCondition(t *testing.T) {
	ctx := context.Background()

	dbSession := testExpectedPowerShelfInitDB(t)
	defer dbSession.Close()

	testExpectedPowerShelfSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	st := cwu.TestBuildSite(t, dbSession, ip, "test-site-race", cdbm.SiteStatusRegistered, nil, ipu)

	// Create an ExpectedPowerShelf: newly create record will have a timestamp within race condition window
	epsDAO := cdbm.NewExpectedPowerShelfDAO(dbSession)
	recentEPS, err := epsDAO.Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(),
		SiteID:               st.ID,
		BmcMacAddress:        "00:11:22:33:44:AA",
		ShelfSerialNumber:    "SHELF-SN-RECENT",
		CreatedBy:            ipu.ID,
	})
	assert.NoError(t, err)

	tSiteClientPool := testTemporalSiteClientPool(t)

	mei := ManageExpectedPowerShelf{
		dbSession:      dbSession,
		siteClientPool: tSiteClientPool,
	}

	// Send inventory without this power shelf - it should NOT be deleted due to race condition
	inventory := &corev1.ExpectedPowerShelfInventory{
		ExpectedPowerShelves: []*corev1.ExpectedPowerShelf{},
		Timestamp:            timestamppb.Now(),
		InventoryStatus:      corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
	}

	err = mei.UpdateExpectedPowerShelvesInDB(ctx, st.ID, inventory)
	assert.NoError(t, err)

	// Verify the power shelf was NOT deleted
	filterInput := cdbm.ExpectedPowerShelfFilterInput{SiteIDs: []uuid.UUID{st.ID}}
	allPowerShelves, _, gerr := epsDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	assert.NoError(t, gerr)

	// Check if the recent power shelf still exists
	found := false
	for _, eps := range allPowerShelves {
		if eps.ID == recentEPS.ID {
			found = true
			break
		}
	}
	assert.True(t, found, "Recently updated ExpectedPowerShelf should NOT be deleted due to race condition")
}

func TestNewManageExpectedPowerShelf(t *testing.T) {
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
		want ManageExpectedPowerShelf
	}{
		{
			name: "test new ManageExpectedPowerShelf instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageExpectedPowerShelf{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewManageExpectedPowerShelf(tt.args.dbSession, tt.args.siteClientPool)
			assert.Equal(t, tt.want.dbSession, got.dbSession, "dbSession should match")
			assert.Equal(t, tt.want.siteClientPool, got.siteClientPool, "siteClientPool should match")
		})
	}
}
