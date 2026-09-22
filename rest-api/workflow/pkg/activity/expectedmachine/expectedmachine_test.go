// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedmachine

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
	"github.com/stretchr/testify/require"
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

func testExpectedMachineInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testExpectedMachineSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create SKU table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SKU)(nil))
	assert.Nil(t, err)
	// create ExpectedMachine table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.ExpectedMachine)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
}

func TestManageExpectedMachine_UpdateExpectedMachinesInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := testExpectedMachineInitDB(t)
	defer dbSession.Close()

	testExpectedMachineSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	// Build Sites
	st := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	st2 := cwu.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)
	st3 := cwu.TestBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, nil, ipu)
	st4 := cwu.TestBuildSite(t, dbSession, ip, "test-site-linked-machine", cdbm.SiteStatusRegistered, nil, ipu)

	// Build ExpectedMachine inventory that is paginated
	// Generate data for 34 ExpectedMachines reported from Site Agent while Cloud has 38 ExpectedMachines
	pagedExpectedMachines := []*cdbm.ExpectedMachine{}
	pagedInvIds := []string{}

	emDAO := cdbm.NewExpectedMachineDAO(dbSession)
	for i := 0; i < 38; i++ {
		emID := uuid.New()
		// Add labels to some machines to test label handling
		var labels map[string]string
		if i%5 == 0 {
			labels = map[string]string{
				"rack":     fmt.Sprintf("rack-%d", i/5),
				"position": fmt.Sprintf("pos-%d", i),
			}
		}
		em, cerr := emDAO.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
			ExpectedMachineID:        emID,
			SiteID:                   st.ID,
			BmcMacAddress:            fmt.Sprintf("00:11:22:33:44:%02d", i),
			ChassisSerialNumber:      fmt.Sprintf("SN-%d", i),
			SkuID:                    nil,
			FallbackDpuSerialNumbers: []string{fmt.Sprintf("DPU-%d", i)},
			Labels:                   labels,
			CreatedBy:                ipu.ID,
		})
		assert.NoError(t, cerr)

		// Update creation and update timestamp to be earlier than inventory processing interval
		_, uerr := dbSession.DB.Exec("UPDATE expected_machine SET created = ?, updated = ? WHERE id = ?",
			time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)),
			time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)),
			em.ID.String())
		assert.NoError(t, uerr)

		pagedExpectedMachines = append(pagedExpectedMachines, em)
		pagedInvIds = append(pagedInvIds, em.ID.String())
	}

	expectedMachinesToUpdate := []*cdbm.ExpectedMachine{}
	pagedCtrlExpectedMachines := []*corev1.ExpectedMachine{}

	for i := 0; i < 34; i++ {
		ctrlExpectedMachine := &corev1.ExpectedMachine{
			Id:                       &corev1.UUID{Value: pagedExpectedMachines[i].ID.String()},
			BmcMacAddress:            pagedExpectedMachines[i].BmcMacAddress,
			ChassisSerialNumber:      pagedExpectedMachines[i].ChassisSerialNumber,
			SkuId:                    pagedExpectedMachines[i].SkuID,
			FallbackDpuSerialNumbers: pagedExpectedMachines[i].FallbackDpuSerialNumbers,
		}

		// Add labels to controller expected machines
		if i%5 == 0 {
			ctrlExpectedMachine.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "rack", Value: cutil.GetPtr(fmt.Sprintf("rack-%d", i/5))},
					{Key: "position", Value: cutil.GetPtr(fmt.Sprintf("pos-%d", i))},
				},
			}
		}

		// Have entries that need updates
		if i%3 == 0 {
			if i < 20 {
				ctrlExpectedMachine.BmcMacAddress = fmt.Sprintf("00:11:22:33:55:%02d", i) // Changed MAC
				expectedMachinesToUpdate = append(expectedMachinesToUpdate, pagedExpectedMachines[i])
			}
		}

		// Test label updates: add/modify labels for some machines
		switch i {
		case 1:
			// Add labels to a machine that didn't have them before
			ctrlExpectedMachine.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "new-label", Value: cutil.GetPtr("new-value")},
				},
			}
			expectedMachinesToUpdate = append(expectedMachinesToUpdate, pagedExpectedMachines[i])
		case 5:
			// Modify existing labels
			ctrlExpectedMachine.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "rack", Value: cutil.GetPtr(fmt.Sprintf("rack-updated-%d", i/5))},
					{Key: "position", Value: cutil.GetPtr(fmt.Sprintf("pos-%d", i))},
					{Key: "status", Value: cutil.GetPtr("active")},
				},
			}
			expectedMachinesToUpdate = append(expectedMachinesToUpdate, pagedExpectedMachines[i])
		case 10:
			// Remove labels (set to empty labels array)
			ctrlExpectedMachine.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{},
			}
			expectedMachinesToUpdate = append(expectedMachinesToUpdate, pagedExpectedMachines[i])
		case 15:
			// Remove labels (set metadata to nil)
			ctrlExpectedMachine.Metadata = nil
			expectedMachinesToUpdate = append(expectedMachinesToUpdate, pagedExpectedMachines[i])
		}

		pagedCtrlExpectedMachines = append(pagedCtrlExpectedMachines, ctrlExpectedMachine)
	}

	expectedMachinesToDelete := pagedExpectedMachines[34:38]

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
		ctx                      context.Context
		siteID                   uuid.UUID
		expectedMachineInventory *corev1.ExpectedMachineInventory
	}

	tests := []struct {
		name                     string
		fields                   fields
		args                     args
		expectedMachinesToUpdate []*cdbm.ExpectedMachine
		expectedMachinesToDelete []*cdbm.ExpectedMachine
		wantErr                  bool
	}{
		{
			name: "test ExpectedMachine inventory processing error, nil inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:                      ctx,
				siteID:                   st.ID,
				expectedMachineInventory: nil,
			},
			wantErr: true,
		},
		{
			name: "test ExpectedMachine inventory processing error, non-existent Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				expectedMachineInventory: &corev1.ExpectedMachineInventory{
					ExpectedMachines: []*corev1.ExpectedMachine{},
				},
			},
			wantErr: true,
		},
		{
			name: "test ExpectedMachine inventory processing, failed inventory status",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedMachineInventory: &corev1.ExpectedMachineInventory{
					ExpectedMachines: []*corev1.ExpectedMachine{},
					Timestamp:        timestamppb.Now(),
					InventoryStatus:  corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
				},
			},
			wantErr: false,
		},
		{
			name: "test paged ExpectedMachine inventory processing, empty inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				expectedMachineInventory: &corev1.ExpectedMachineInventory{
					ExpectedMachines: []*corev1.ExpectedMachine{},
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
			name: "test paged ExpectedMachine inventory processing, first page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedMachineInventory: &corev1.ExpectedMachineInventory{
					ExpectedMachines: pagedCtrlExpectedMachines[0:20],
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
			expectedMachinesToUpdate: expectedMachinesToUpdate,
			expectedMachinesToDelete: []*cdbm.ExpectedMachine{},
		},
		{
			name: "test paged ExpectedMachine inventory processing, last page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedMachineInventory: &corev1.ExpectedMachineInventory{
					ExpectedMachines: pagedCtrlExpectedMachines[20:34],
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
			expectedMachinesToUpdate: []*cdbm.ExpectedMachine{},
			expectedMachinesToDelete: expectedMachinesToDelete,
		},
		{
			name: "test non-paged ExpectedMachine inventory processing",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				expectedMachineInventory: &corev1.ExpectedMachineInventory{
					ExpectedMachines: []*corev1.ExpectedMachine{
						{
							Id:                  &corev1.UUID{Value: uuid.New().String()},
							BmcMacAddress:       "00:11:22:33:44:FF",
							ChassisSerialNumber: "SN-NEW-1",
							SkuId:               nil,
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
			expectedMachinesToUpdate: []*cdbm.ExpectedMachine{},
			expectedMachinesToDelete: []*cdbm.ExpectedMachine{},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mei := ManageExpectedMachine{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
			}

			cwu.TestInventoryAgeUpdatedTimestamp(tt.args.ctx, t, dbSession, (*cdbm.ExpectedMachine)(nil))

			err := mei.UpdateExpectedMachinesInDB(tt.args.ctx, tt.args.siteID, tt.args.expectedMachineInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			// Verify updates by fetching all machines for the site
			emDAO := cdbm.NewExpectedMachineDAO(dbSession)
			filterInput := cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{tt.args.siteID}}
			allMachines, _, gerr := emDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			assert.NoError(t, gerr)

			// Build a map of machines by ID for easy lookup
			machinesByID := map[uuid.UUID]*cdbm.ExpectedMachine{}
			for i := range allMachines {
				machinesByID[allMachines[i].ID] = &allMachines[i]
			}

			for _, em := range tt.expectedMachinesToUpdate {
				updated := machinesByID[em.ID]
				assert.NotNil(t, updated, fmt.Sprintf("ExpectedMachine %v should exist", em.ID))
				// Find the corresponding controller machine
				var ctrlEM *corev1.ExpectedMachine
				for _, cem := range tt.args.expectedMachineInventory.ExpectedMachines {
					if cem.Id.Value == em.ID.String() {
						ctrlEM = cem
						break
					}
				}
				if ctrlEM != nil {
					assert.Equal(t, ctrlEM.BmcMacAddress, updated.BmcMacAddress,
						fmt.Sprintf("ExpectedMachine %v should have been updated", em.ID))

					// Verify labels are updated correctly
					var expectedLabels cdbm.Labels
					expectedLabels.FromProto(ctrlEM.Metadata.GetLabels())
					// Both nil and empty maps should be treated as equivalent (no labels)
					if len(expectedLabels) == 0 && len(updated.Labels) == 0 {
						// Both are effectively empty, which is correct
					} else {
						assert.Equal(t, expectedLabels, updated.Labels,
							fmt.Sprintf("ExpectedMachine %v labels should match", em.ID))
					}
				}
			}

			// Verify deletions
			for _, em := range tt.expectedMachinesToDelete {
				deleted := machinesByID[em.ID]
				assert.Nil(t, deleted, fmt.Sprintf("ExpectedMachine %v should have been deleted", em.ID))
			}

			// Verify newly created machines have correct labels
			for _, cem := range tt.args.expectedMachineInventory.ExpectedMachines {
				emID, perr := uuid.Parse(cem.Id.Value)
				assert.NoError(t, perr)
				created := machinesByID[emID]
				if created != nil {
					var expectedLabels cdbm.Labels
					expectedLabels.FromProto(cem.Metadata.GetLabels())
					// Both nil and empty maps should be treated as equivalent (no labels)
					if len(expectedLabels) == 0 && len(created.Labels) == 0 {
						// Both are effectively empty, which is correct
					} else {
						assert.Equal(t, expectedLabels, created.Labels,
							fmt.Sprintf("ExpectedMachine %v labels should match on creation", emID))
					}
				}
			}
		})
	}

	t.Run("defer association until the linked machine exists in REST", func(t *testing.T) {
		emID := uuid.New()
		bmcMAC := "00:11:22:33:77:01"
		predictedMachineID := "fm100p-predicted-host"
		inventory := &corev1.ExpectedMachineInventory{
			ExpectedMachines: []*corev1.ExpectedMachine{
				{
					Id:                  &corev1.UUID{Value: emID.String()},
					BmcMacAddress:       bmcMAC,
					ChassisSerialNumber: "SN-LINKED-MACHINE",
				},
			},
			LinkedMachines: []*corev1.LinkedExpectedMachine{
				{
					BmcMacAddress: bmcMAC,
					MachineId:     &corev1.MachineId{Id: predictedMachineID},
				},
			},
			Timestamp:       timestamppb.Now(),
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		}

		mei := ManageExpectedMachine{dbSession: dbSession}
		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedMachine)(nil))
		err := mei.UpdateExpectedMachinesInDB(ctx, st4.ID, inventory)
		require.NoError(t, err)

		reconciled, err := emDAO.Get(ctx, nil, emID, nil, false)
		require.NoError(t, err)
		assert.Nil(t, reconciled.MachineID)

		permanentMachine := cwu.TestBuildMachine(t, dbSession, ip.ID, st4.ID, nil, cutil.GetPtr(false), cdbm.MachineStatusReady)
		inventory.LinkedMachines[0].MachineId.Id = permanentMachine.ID

		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedMachine)(nil))
		err = mei.UpdateExpectedMachinesInDB(ctx, st4.ID, inventory)
		require.NoError(t, err)

		reconciled, err = emDAO.Get(ctx, nil, emID, nil, false)
		require.NoError(t, err)
		assert.Equal(t, &permanentMachine.ID, reconciled.MachineID)
	})
}

// TestManageExpectedMachine_UpdateRespectsStaleInventoryThreshold covers the gate shared by every
// inventory activity that overwrites provider-set fields: an inventory collected before an edge
// edit must not undo it. ExpectedMachine stands in for the rest, which apply the same check.
func TestManageExpectedMachine_UpdateRespectsStaleInventoryThreshold(t *testing.T) {
	testCases := []struct {
		name         string
		writtenAgo   time.Duration
		wantSerial   string
		wantLabelSet bool
	}{
		{
			name:         "a recently written ExpectedMachine keeps its stored values",
			writtenAgo:   time.Second,
			wantSerial:   "SET-THROUGH-API",
			wantLabelSet: true,
		},
		{
			name:       "an ExpectedMachine untouched for longer than the interval takes the reported values",
			writtenAgo: 2 * cutil.DefaultInventoryReceiptInterval,
			wantSerial: "REPORTED-BY-SITE",
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()

			dbSession := testExpectedMachineInitDB(t)
			defer dbSession.Close()
			testExpectedMachineSetupSchema(t, dbSession)

			ipOrg := "test-provider-org"
			ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{"FORGE_PROVIDER_ADMIN"})
			ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
			st := cwu.TestBuildSite(t, dbSession, ip, "test-site-stale", cdbm.SiteStatusRegistered, nil, ipu)

			emDAO := cdbm.NewExpectedMachineDAO(dbSession)
			emID := uuid.New()
			_, err := emDAO.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
				ExpectedMachineID:   emID,
				SiteID:              st.ID,
				BmcMacAddress:       "00:11:22:33:99:01",
				ChassisSerialNumber: "SET-THROUGH-API",
				Labels:              map[string]string{"owner": "provider"},
				CreatedBy:           ipu.ID,
			})
			require.NoError(t, err)

			_, err = dbSession.DB.NewUpdate().
				Model((*cdbm.ExpectedMachine)(nil)).
				Set("updated = ?", time.Now().Add(-tc.writtenAgo)).
				Where("id = ?", emID).
				Exec(ctx)
			require.NoError(t, err)

			mei := ManageExpectedMachine{dbSession: dbSession}
			err = mei.UpdateExpectedMachinesInDB(ctx, st.ID, &corev1.ExpectedMachineInventory{
				ExpectedMachines: []*corev1.ExpectedMachine{{
					Id:                  &corev1.UUID{Value: emID.String()},
					BmcMacAddress:       "00:11:22:33:99:01",
					ChassisSerialNumber: "REPORTED-BY-SITE",
				}},
				Timestamp:       timestamppb.Now(),
				InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			})
			require.NoError(t, err)

			got, err := emDAO.Get(ctx, nil, emID, nil, false)
			require.NoError(t, err)
			assert.Equal(t, tc.wantSerial, got.ChassisSerialNumber)
			if tc.wantLabelSet {
				assert.Equal(t, cdbm.Labels{"owner": "provider"}, got.Labels, "a deferred update must leave labels alone")
			}
		})
	}
}

func TestManageExpectedMachine_UpdateExpectedMachinesInDB_BmcIpAddress(t *testing.T) {
	ctx := context.Background()

	dbSession := testExpectedMachineInitDB(t)
	defer dbSession.Close()

	testExpectedMachineSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	st := cwu.TestBuildSite(t, dbSession, ip, "test-site-bmc-ip", cdbm.SiteStatusRegistered, nil, ipu)

	type existingMachineCase struct {
		name          string
		initialBmcIP  *string
		reportedBmcIP *string
	}

	tests := []existingMachineCase{
		{
			name:          "set previously unset BMC IP address",
			initialBmcIP:  nil,
			reportedBmcIP: cutil.GetPtr("192.0.2.10"),
		},
		{
			name:          "change existing BMC IP address",
			initialBmcIP:  cutil.GetPtr("192.0.2.20"),
			reportedBmcIP: cutil.GetPtr("192.0.2.21"),
		},
		{
			name:          "clear existing BMC IP address",
			initialBmcIP:  cutil.GetPtr("192.0.2.30"),
			reportedBmcIP: nil,
		},
	}

	emDAO := cdbm.NewExpectedMachineDAO(dbSession)
	existingIDs := make([]uuid.UUID, len(tests))
	reportedMachines := make([]*corev1.ExpectedMachine, 0, len(tests)+1)
	preservedName := "preserve-this-name"
	for i, tt := range tests {
		emID := uuid.New()
		existingIDs[i] = emID
		bmcMAC := fmt.Sprintf("00:11:22:33:66:%02d", i)
		chassisSerialNumber := fmt.Sprintf("BMC-IP-SN-%d", i)
		_, err := emDAO.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
			ExpectedMachineID:   emID,
			SiteID:              st.ID,
			BmcMacAddress:       bmcMAC,
			ChassisSerialNumber: chassisSerialNumber,
			BmcIpAddress:        tt.initialBmcIP,
			Name:                &preservedName,
			CreatedBy:           ipu.ID,
		})
		require.NoError(t, err)

		reportedMachines = append(reportedMachines, &corev1.ExpectedMachine{
			Id:                  &corev1.UUID{Value: emID.String()},
			BmcMacAddress:       bmcMAC,
			ChassisSerialNumber: chassisSerialNumber,
			BmcIpAddress:        tt.reportedBmcIP,
		})
	}

	createdID := uuid.New()
	createdBmcIP := "192.0.2.40"
	reportedMachines = append(reportedMachines, &corev1.ExpectedMachine{
		Id:                  &corev1.UUID{Value: createdID.String()},
		BmcMacAddress:       "00:11:22:33:66:03",
		ChassisSerialNumber: "BMC-IP-SN-3",
		BmcIpAddress:        &createdBmcIP,
	})

	mei := ManageExpectedMachine{dbSession: dbSession}
	cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedMachine)(nil))
	err := mei.UpdateExpectedMachinesInDB(ctx, st.ID, &corev1.ExpectedMachineInventory{
		ExpectedMachines: reportedMachines,
		Timestamp:        timestamppb.Now(),
		InventoryStatus:  corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
	})
	require.NoError(t, err)

	for i, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			updated, err := emDAO.Get(ctx, nil, existingIDs[i], nil, false)
			require.NoError(t, err)
			assert.Equal(t, tt.reportedBmcIP, updated.BmcIpAddress)
			assert.Equal(t, &preservedName, updated.Name, "reconciling the BMC IP must preserve unrelated fields")
		})
	}

	created, err := emDAO.Get(ctx, nil, createdID, nil, false)
	require.NoError(t, err)
	assert.Equal(t, &createdBmcIP, created.BmcIpAddress)

	t.Run("rolls back a clear when the accompanying update fails", func(t *testing.T) {
		emID := uuid.New()
		originalBmcIP := "192.0.2.50"
		_, err := emDAO.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
			ExpectedMachineID:   emID,
			SiteID:              st.ID,
			BmcMacAddress:       "00:11:22:33:66:04",
			ChassisSerialNumber: "BMC-IP-SN-4",
			BmcIpAddress:        &originalBmcIP,
			Name:                &preservedName,
			CreatedBy:           ipu.ID,
		})
		require.NoError(t, err)

		nonexistentSkuID := "nonexistent-sku"
		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedMachine)(nil))
		err = mei.UpdateExpectedMachinesInDB(ctx, st.ID, &corev1.ExpectedMachineInventory{
			ExpectedMachines: []*corev1.ExpectedMachine{
				{
					Id:                  &corev1.UUID{Value: emID.String()},
					BmcMacAddress:       "00:11:22:33:66:04",
					ChassisSerialNumber: "BMC-IP-SN-4",
					BmcIpAddress:        nil,
					SkuId:               &nonexistentSkuID,
				},
			},
			Timestamp:       timestamppb.Now(),
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		})
		require.NoError(t, err, "per-record reconciliation errors remain best effort")

		unchanged, err := emDAO.Get(ctx, nil, emID, nil, false)
		require.NoError(t, err)
		assert.Equal(t, &originalBmcIP, unchanged.BmcIpAddress)
		assert.Equal(t, &preservedName, unchanged.Name)
	})
}

func TestManageExpectedMachine_UpdateExpectedMachinesInDB_RaceCondition(t *testing.T) {
	ctx := context.Background()

	dbSession := testExpectedMachineInitDB(t)
	defer dbSession.Close()

	testExpectedMachineSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	st := cwu.TestBuildSite(t, dbSession, ip, "test-site-race", cdbm.SiteStatusRegistered, nil, ipu)

	// Create an ExpectedMachine: newly create record will have a timestamp within race condition window
	emDAO := cdbm.NewExpectedMachineDAO(dbSession)
	recentEM, err := emDAO.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              st.ID,
		BmcMacAddress:       "00:11:22:33:44:AA",
		ChassisSerialNumber: "SN-RECENT",
		CreatedBy:           ipu.ID,
	})
	assert.NoError(t, err)

	tSiteClientPool := testTemporalSiteClientPool(t)

	mei := ManageExpectedMachine{
		dbSession:      dbSession,
		siteClientPool: tSiteClientPool,
	}

	// Send inventory without this machine - it should NOT be deleted due to race condition
	inventory := &corev1.ExpectedMachineInventory{
		ExpectedMachines: []*corev1.ExpectedMachine{},
		Timestamp:        timestamppb.Now(),
		InventoryStatus:  corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
	}

	err = mei.UpdateExpectedMachinesInDB(ctx, st.ID, inventory)
	assert.NoError(t, err)

	// Verify the machine was NOT deleted
	filterInput := cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{st.ID}}
	allMachines, _, gerr := emDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	assert.NoError(t, gerr)

	// Check if the recent machine still exists
	found := false
	for _, em := range allMachines {
		if em.ID == recentEM.ID {
			found = true
			break
		}
	}
	assert.True(t, found, "Recently updated ExpectedMachine should NOT be deleted due to race condition")
}

func TestNewManageExpectedMachine(t *testing.T) {
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
		want ManageExpectedMachine
	}{
		{
			name: "test new ManageExpectedMachine instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageExpectedMachine{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewManageExpectedMachine(tt.args.dbSession, tt.args.siteClientPool)
			assert.Equal(t, tt.want.dbSession, got.dbSession, "dbSession should match")
			assert.Equal(t, tt.want.siteClientPool, got.siteClientPool, "siteClientPool should match")
		})
	}
}
