// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedswitch

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

func testExpectedSwitchInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testExpectedSwitchSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create ExpectedSwitch table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.ExpectedSwitch)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
}

func TestManageExpectedSwitch_UpdateExpectedSwitchesInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := testExpectedSwitchInitDB(t)
	defer dbSession.Close()

	testExpectedSwitchSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	// Build Sites
	st := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	st2 := cwu.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)
	st3 := cwu.TestBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, nil, ipu)

	// Build ExpectedSwitch inventory that is paginated
	// Generate data for 34 ExpectedSwitches reported from Site Agent while Cloud has 38 ExpectedSwitches
	pagedExpectedSwitches := []*cdbm.ExpectedSwitch{}
	pagedInvIds := []string{}

	esDAO := cdbm.NewExpectedSwitchDAO(dbSession)
	for i := 0; i < 38; i++ {
		esID := uuid.New()
		// Add labels to some switches to test label handling
		var labels map[string]string
		if i%5 == 0 {
			labels = map[string]string{
				"rack":     fmt.Sprintf("rack-%d", i/5),
				"position": fmt.Sprintf("pos-%d", i),
			}
		}
		// Give one switch NVOS MACs so the clear-on-reconcile path is exercised
		var nvosMacs []string
		if i == 4 {
			nvosMacs = []string{"aa:bb:cc:dd:ee:04"}
		}
		es, cerr := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
			ExpectedSwitchID:   esID,
			SiteID:             st.ID,
			BmcMacAddress:      fmt.Sprintf("00:11:22:33:44:%02d", i),
			SwitchSerialNumber: fmt.Sprintf("SW-SN-%d", i),
			NvosMacAddresses:   nvosMacs,
			Labels:             labels,
			CreatedBy:          ipu.ID,
		})
		assert.NoError(t, cerr)

		// Update creation and update timestamp to be earlier than inventory processing interval
		_, uerr := dbSession.DB.Exec("UPDATE expected_switch SET created = ?, updated = ? WHERE id = ?",
			time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval*2)),
			time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval*2)),
			es.ID.String())
		assert.NoError(t, uerr)

		pagedExpectedSwitches = append(pagedExpectedSwitches, es)
		pagedInvIds = append(pagedInvIds, es.ID.String())
	}

	expectedSwitchesToUpdate := []*cdbm.ExpectedSwitch{}
	pagedCtrlExpectedSwitches := []*corev1.ExpectedSwitch{}

	for i := 0; i < 34; i++ {
		ctrlExpectedSwitch := &corev1.ExpectedSwitch{
			ExpectedSwitchId:   &corev1.UUID{Value: pagedExpectedSwitches[i].ID.String()},
			BmcMacAddress:      pagedExpectedSwitches[i].BmcMacAddress,
			SwitchSerialNumber: pagedExpectedSwitches[i].SwitchSerialNumber,
		}

		// Add labels to controller expected switches
		if i%5 == 0 {
			ctrlExpectedSwitch.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "rack", Value: cutil.GetPtr(fmt.Sprintf("rack-%d", i/5))},
					{Key: "position", Value: cutil.GetPtr(fmt.Sprintf("pos-%d", i))},
				},
			}
		}

		// Have entries that need updates
		if i%3 == 0 {
			if i < 20 {
				ctrlExpectedSwitch.BmcMacAddress = fmt.Sprintf("00:11:22:33:55:%02d", i) // Changed MAC
				expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
			}
		}

		// Test label updates: add/modify labels for some switches
		if i == 1 {
			// Add labels to a switch that didn't have them before
			ctrlExpectedSwitch.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "new-label", Value: cutil.GetPtr("new-value")},
				},
			}
			expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
		} else if i == 2 {
			// Report NVOS MAC addresses for a switch that didn't have them before
			ctrlExpectedSwitch.NvosMacAddresses = []string{"aa:bb:cc:dd:ee:02", "aa:bb:cc:dd:ee:03"}
			expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
		} else if i == 4 {
			// NVOS MACs cleared on the Site: reported without any, DB has one
			expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
		} else if i == 5 {
			// Modify existing labels
			ctrlExpectedSwitch.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "rack", Value: cutil.GetPtr(fmt.Sprintf("rack-updated-%d", i/5))},
					{Key: "position", Value: cutil.GetPtr(fmt.Sprintf("pos-%d", i))},
					{Key: "status", Value: cutil.GetPtr("active")},
				},
			}
			expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
		} else if i == 10 {
			// Remove labels (set to empty labels array)
			ctrlExpectedSwitch.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{},
			}
			expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
		} else if i == 15 {
			// Remove labels (set metadata to nil)
			ctrlExpectedSwitch.Metadata = nil
			expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
		}

		pagedCtrlExpectedSwitches = append(pagedCtrlExpectedSwitches, ctrlExpectedSwitch)
	}

	expectedSwitchesToDelete := pagedExpectedSwitches[34:38]

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
		ctx                     context.Context
		siteID                  uuid.UUID
		expectedSwitchInventory *corev1.ExpectedSwitchInventory
	}

	tests := []struct {
		name                     string
		fields                   fields
		args                     args
		expectedSwitchesToUpdate []*cdbm.ExpectedSwitch
		expectedSwitchesToDelete []*cdbm.ExpectedSwitch
		wantErr                  bool
	}{
		{
			name: "test ExpectedSwitch inventory processing error, nil inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:                     ctx,
				siteID:                  st.ID,
				expectedSwitchInventory: nil,
			},
			wantErr: true,
		},
		{
			name: "test ExpectedSwitch inventory processing error, non-existent Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				expectedSwitchInventory: &corev1.ExpectedSwitchInventory{
					ExpectedSwitches: []*corev1.ExpectedSwitch{},
				},
			},
			wantErr: true,
		},
		{
			name: "test ExpectedSwitch inventory processing, failed inventory status",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedSwitchInventory: &corev1.ExpectedSwitchInventory{
					ExpectedSwitches: []*corev1.ExpectedSwitch{},
					Timestamp:        timestamppb.Now(),
					InventoryStatus:  corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
				},
			},
			wantErr: false,
		},
		{
			name: "test paged ExpectedSwitch inventory processing, empty inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				expectedSwitchInventory: &corev1.ExpectedSwitchInventory{
					ExpectedSwitches: []*corev1.ExpectedSwitch{},
					Timestamp:        timestamppb.Now(),
					InventoryStatus:  corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
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
			name: "test paged ExpectedSwitch inventory processing, first page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedSwitchInventory: &corev1.ExpectedSwitchInventory{
					ExpectedSwitches: pagedCtrlExpectedSwitches[0:20],
					Timestamp:        timestamppb.Now(),
					InventoryStatus:  corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  2,
						PageSize:    20,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			expectedSwitchesToUpdate: expectedSwitchesToUpdate,
			expectedSwitchesToDelete: []*cdbm.ExpectedSwitch{},
		},
		{
			name: "test paged ExpectedSwitch inventory processing, last page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedSwitchInventory: &corev1.ExpectedSwitchInventory{
					ExpectedSwitches: pagedCtrlExpectedSwitches[20:34],
					Timestamp:        timestamppb.Now(),
					InventoryStatus:  corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 2,
						TotalPages:  2,
						PageSize:    20,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			expectedSwitchesToUpdate: []*cdbm.ExpectedSwitch{},
			expectedSwitchesToDelete: expectedSwitchesToDelete,
		},
		{
			name: "test non-paged ExpectedSwitch inventory processing",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				expectedSwitchInventory: &corev1.ExpectedSwitchInventory{
					ExpectedSwitches: []*corev1.ExpectedSwitch{
						{
							ExpectedSwitchId:   &corev1.UUID{Value: uuid.New().String()},
							BmcMacAddress:      "00:11:22:33:44:FF",
							SwitchSerialNumber: "SW-SN-NEW-1",
							NvosMacAddresses:   []string{"aa:bb:cc:dd:ee:f1"},
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
			expectedSwitchesToUpdate: []*cdbm.ExpectedSwitch{},
			expectedSwitchesToDelete: []*cdbm.ExpectedSwitch{},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mei := ManageExpectedSwitch{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
			}

			err := mei.UpdateExpectedSwitchesInDB(tt.args.ctx, tt.args.siteID, tt.args.expectedSwitchInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			// Verify updates by fetching all switches for the site
			esDAO := cdbm.NewExpectedSwitchDAO(dbSession)
			filterInput := cdbm.ExpectedSwitchFilterInput{SiteIDs: []uuid.UUID{tt.args.siteID}}
			allSwitches, _, gerr := esDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			assert.NoError(t, gerr)

			// Build a map of switches by ID for easy lookup
			switchesByID := map[uuid.UUID]*cdbm.ExpectedSwitch{}
			for i := range allSwitches {
				switchesByID[allSwitches[i].ID] = &allSwitches[i]
			}

			for _, es := range tt.expectedSwitchesToUpdate {
				updated := switchesByID[es.ID]
				assert.NotNil(t, updated, fmt.Sprintf("ExpectedSwitch %v should exist", es.ID))
				// Find the corresponding controller switch
				var ctrlES *corev1.ExpectedSwitch
				for _, ces := range tt.args.expectedSwitchInventory.ExpectedSwitches {
					if ces.ExpectedSwitchId.Value == es.ID.String() {
						ctrlES = ces
						break
					}
				}
				if ctrlES != nil {
					assert.Equal(t, ctrlES.BmcMacAddress, updated.BmcMacAddress,
						fmt.Sprintf("ExpectedSwitch %v should have been updated", es.ID))
					// Both nil and empty lists should be treated as equivalent (no NVOS MACs)
					if len(ctrlES.NvosMacAddresses) != 0 || len(updated.NvosMacAddresses) != 0 {
						assert.Equal(t, ctrlES.NvosMacAddresses, updated.NvosMacAddresses,
							fmt.Sprintf("ExpectedSwitch %v NVOS MAC addresses should match", es.ID))
					}

					// Verify labels are updated correctly
					var expectedLabels cdbm.Labels
					expectedLabels.FromProto(ctrlES.Metadata.GetLabels())
					// Both nil and empty maps should be treated as equivalent (no labels)
					if len(expectedLabels) == 0 && len(updated.Labels) == 0 {
						// Both are effectively empty, which is correct
					} else {
						assert.Equal(t, expectedLabels, updated.Labels,
							fmt.Sprintf("ExpectedSwitch %v labels should match", es.ID))
					}
				}
			}

			// Verify deletions
			for _, es := range tt.expectedSwitchesToDelete {
				deleted := switchesByID[es.ID]
				assert.Nil(t, deleted, fmt.Sprintf("ExpectedSwitch %v should have been deleted", es.ID))
			}

			// Verify newly created switches have correct labels and NVOS MAC addresses
			for _, ces := range tt.args.expectedSwitchInventory.ExpectedSwitches {
				esID, perr := uuid.Parse(ces.ExpectedSwitchId.Value)
				assert.NoError(t, perr)
				created := switchesByID[esID]
				if created != nil {
					// Both nil and empty lists should be treated as equivalent (no NVOS MACs)
					if len(ces.NvosMacAddresses) != 0 || len(created.NvosMacAddresses) != 0 {
						assert.Equal(t, ces.NvosMacAddresses, created.NvosMacAddresses,
							fmt.Sprintf("ExpectedSwitch %v NVOS MAC addresses should match", esID))
					}
					var expectedLabels cdbm.Labels
					expectedLabels.FromProto(ces.Metadata.GetLabels())
					// Both nil and empty maps should be treated as equivalent (no labels)
					if len(expectedLabels) == 0 && len(created.Labels) == 0 {
						// Both are effectively empty, which is correct
					} else {
						assert.Equal(t, expectedLabels, created.Labels,
							fmt.Sprintf("ExpectedSwitch %v labels should match on creation", esID))
					}
				}
			}
		})
	}
}

func TestManageExpectedSwitch_UpdateExpectedSwitchesInDB_RaceCondition(t *testing.T) {
	ctx := context.Background()

	dbSession := testExpectedSwitchInitDB(t)
	defer dbSession.Close()

	testExpectedSwitchSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	st := cwu.TestBuildSite(t, dbSession, ip, "test-site-race", cdbm.SiteStatusRegistered, nil, ipu)

	// Create an ExpectedSwitch: newly create record will have a timestamp within race condition window
	esDAO := cdbm.NewExpectedSwitchDAO(dbSession)
	recentES, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             st.ID,
		BmcMacAddress:      "00:11:22:33:44:AA",
		SwitchSerialNumber: "SW-SN-RECENT",
		CreatedBy:          ipu.ID,
	})
	assert.NoError(t, err)

	tSiteClientPool := testTemporalSiteClientPool(t)

	mei := ManageExpectedSwitch{
		dbSession:      dbSession,
		siteClientPool: tSiteClientPool,
	}

	// Send inventory without this switch - it should NOT be deleted due to race condition
	inventory := &corev1.ExpectedSwitchInventory{
		ExpectedSwitches: []*corev1.ExpectedSwitch{},
		Timestamp:        timestamppb.Now(),
		InventoryStatus:  corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
	}

	err = mei.UpdateExpectedSwitchesInDB(ctx, st.ID, inventory)
	assert.NoError(t, err)

	// Verify the switch was NOT deleted
	filterInput := cdbm.ExpectedSwitchFilterInput{SiteIDs: []uuid.UUID{st.ID}}
	allSwitches, _, gerr := esDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	assert.NoError(t, gerr)

	// Check if the recent switch still exists
	found := false
	for _, es := range allSwitches {
		if es.ID == recentES.ID {
			found = true
			break
		}
	}
	assert.True(t, found, "Recently updated ExpectedSwitch should NOT be deleted due to race condition")
}

func TestNewManageExpectedSwitch(t *testing.T) {
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
		want ManageExpectedSwitch
	}{
		{
			name: "test new ManageExpectedSwitch instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageExpectedSwitch{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewManageExpectedSwitch(tt.args.dbSession, tt.args.siteClientPool)
			assert.Equal(t, tt.want.dbSession, got.dbSession, "dbSession should match")
			assert.Equal(t, tt.want.siteClientPool, got.siteClientPool, "siteClientPool should match")
		})
	}
}
