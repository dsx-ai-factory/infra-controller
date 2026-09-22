// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedpowershelf

import (
	"context"
	"fmt"
	"os"
	"testing"
	"time"

	"go.temporal.io/sdk/testsuite"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
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
