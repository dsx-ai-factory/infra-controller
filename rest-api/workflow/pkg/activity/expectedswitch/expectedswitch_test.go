// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedswitch

import (
	"bytes"
	"context"
	"encoding/json"
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
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
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

// protoNvosMacs extracts the ordered NVOS MAC list from a reported switch proto
// so tests can compare it against the flat DB column the activity persists.
func protoNvosMacs(es *corev1.ExpectedSwitch) []string {
	interfaces := es.GetNvosInterfaces()
	if len(interfaces) == 0 {
		return nil
	}
	macs := make([]string, 0, len(interfaces))
	for _, iface := range interfaces {
		macs = append(macs, iface.GetMacAddress())
	}
	return macs
}

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

type testExpectedSwitchReconcileContextKey struct{}

type testExpectedSwitchAfterReadHook struct {
	afterRead    func()
	beforeCommit func()
	rowID        uuid.UUID
}

func (h *testExpectedSwitchAfterReadHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if ctx.Value(testExpectedSwitchReconcileContextKey{}) == h && event.Operation() == "COMMIT" && h.beforeCommit != nil {
		h.beforeCommit()
	}
	return ctx
}

func (h *testExpectedSwitchAfterReadHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
	if h.rowID != uuid.Nil && (ctx.Value(testExpectedSwitchReconcileContextKey{}) != h || !strings.Contains(event.Query, h.rowID.String())) {
		return
	}
	if h.afterRead == nil || event.Err != nil || event.Operation() != "SELECT" ||
		strings.HasPrefix(event.Query, "SELECT count(") || !strings.Contains(event.Query, `FROM "expected_switch"`) {
		return
	}
	afterRead := h.afterRead
	h.afterRead = nil
	afterRead()
}

type testExpectedSwitchBeforeDeleteHook struct {
	rowID        uuid.UUID
	beforeDelete func()
	fired        bool
	err          error
}

func (h *testExpectedSwitchBeforeDeleteHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if !h.fired && event.Operation() == "DELETE" && strings.Contains(event.Query, `FROM "expected_switch"`) && strings.Contains(event.Query, h.rowID.String()) {
		h.fired = true
		if h.beforeDelete != nil {
			h.beforeDelete()
		}
	}
	return ctx
}

func (h *testExpectedSwitchBeforeDeleteHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
	if event.Operation() == "DELETE" && strings.Contains(event.Query, `FROM "expected_switch"`) && strings.Contains(event.Query, h.rowID.String()) {
		h.err = event.Err
	}
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
			time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)),
			time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)),
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
		switch i {
		case 1:
			// Add labels to a switch that didn't have them before
			ctrlExpectedSwitch.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "new-label", Value: cutil.GetPtr("new-value")},
				},
			}
			expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
		case 2:
			// Report NVOS MAC addresses for a switch that didn't have them before
			ctrlExpectedSwitch.NvosInterfaces = []*corev1.ExpectedNvosInterface{
				{MacAddress: "aa:bb:cc:dd:ee:02", IsPrimary: true},
				{MacAddress: "aa:bb:cc:dd:ee:03", IsPrimary: false},
			}
			expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
		case 4:
			// NVOS MACs cleared on the Site: reported without any, DB has one
			expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
		case 5:
			// Modify existing labels
			ctrlExpectedSwitch.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "rack", Value: cutil.GetPtr(fmt.Sprintf("rack-updated-%d", i/5))},
					{Key: "position", Value: cutil.GetPtr(fmt.Sprintf("pos-%d", i))},
					{Key: "status", Value: cutil.GetPtr("active")},
				},
			}
			expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
		case 10:
			// Remove labels (set to empty labels array)
			ctrlExpectedSwitch.Metadata = &corev1.Metadata{
				Labels: []*corev1.Label{},
			}
			expectedSwitchesToUpdate = append(expectedSwitchesToUpdate, pagedExpectedSwitches[i])
		case 15:
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

	unsupportedNewID := uuid.New()

	tests := []struct {
		name                     string
		fields                   fields
		args                     args
		expectedSwitchesToUpdate []*cdbm.ExpectedSwitch
		expectedSwitchesToDelete []*cdbm.ExpectedSwitch
		unchangedID              uuid.UUID
		notCreatedID             uuid.UUID
		wantWarning              string
		wantInventoryStatus      string
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
					InventoryStatus:  corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
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
			unchangedID: pagedExpectedSwitches[7].ID,
			wantErr:     false,
			wantWarning: "received failed inventory status from Site Agent, skipping inventory processing",
		},
		{
			name: "test ExpectedSwitch inventory processing, unspecified inventory status",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:                     ctx,
				siteID:                  st.ID,
				expectedSwitchInventory: &corev1.ExpectedSwitchInventory{},
			},
			unchangedID:         pagedExpectedSwitches[7].ID,
			wantWarning:         "received unsupported inventory status, skipping inventory processing",
			wantInventoryStatus: "INVENTORY_STATUS_UNSPECIFIED",
		},
		{
			name: "test ExpectedSwitch inventory processing, unknown inventory status",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				expectedSwitchInventory: &corev1.ExpectedSwitchInventory{
					InventoryStatus: corev1.InventoryStatus(99),
					ExpectedSwitches: []*corev1.ExpectedSwitch{
						{
							ExpectedSwitchId:   &corev1.UUID{Value: pagedExpectedSwitches[7].ID.String()},
							BmcMacAddress:      "00:11:22:33:66:07",
							SwitchSerialNumber: pagedExpectedSwitches[7].SwitchSerialNumber,
						},
						{
							ExpectedSwitchId:   &corev1.UUID{Value: unsupportedNewID.String()},
							BmcMacAddress:      "00:11:22:33:66:FF",
							SwitchSerialNumber: "SN-UNSUPPORTED",
						},
					},
				},
			},
			unchangedID:         pagedExpectedSwitches[7].ID,
			wantWarning:         "received unsupported inventory status, skipping inventory processing",
			wantInventoryStatus: "99",
			notCreatedID:        unsupportedNewID,
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
			unchangedID:              pagedExpectedSwitches[7].ID,
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
							NvosInterfaces: []*corev1.ExpectedNvosInterface{
								{MacAddress: "aa:bb:cc:dd:ee:f1", IsPrimary: true},
							},
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

			var beforeUnchanged *cdbm.ExpectedSwitch
			if tt.unchangedID != uuid.Nil {
				var err error
				beforeUnchanged, err = esDAO.Get(ctx, nil, tt.unchangedID, nil, false)
				require.NoError(t, err)
				require.Nil(t, beforeUnchanged.Labels)
				require.Nil(t, beforeUnchanged.NvosMacAddresses)
				require.False(t, st.IsTimeWithinStaleInventoryThreshold(beforeUnchanged.Updated))
			}

			var logOutput bytes.Buffer
			if tt.wantWarning != "" {
				originalLogger := log.Logger
				log.Logger = zerolog.New(&logOutput).Level(zerolog.WarnLevel)
				t.Cleanup(func() { log.Logger = originalLogger })
			}

			err := mei.UpdateExpectedSwitchesInDB(tt.args.ctx, tt.args.siteID, tt.args.expectedSwitchInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantWarning != "" {
				var warning map[string]any
				require.NoError(t, json.Unmarshal(logOutput.Bytes(), &warning))
				assert.Equal(t, "warn", warning["level"])
				assert.Equal(t, tt.wantWarning, warning["message"])
				assert.Equal(t, "UpdateExpectedSwitchesInDB", warning["Activity"])
				assert.Equal(t, tt.args.siteID.String(), warning["Site ID"])
				if tt.wantInventoryStatus != "" {
					assert.Equal(t, tt.wantInventoryStatus, warning["inventory_status"])
				}
			}

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

			if tt.notCreatedID != uuid.Nil {
				assert.NotContains(t, switchesByID, tt.notCreatedID)
			}

			if beforeUnchanged != nil {
				unchanged := switchesByID[beforeUnchanged.ID]
				require.NotNil(t, unchanged)
				assert.Equal(t, beforeUnchanged, unchanged, "stored row must keep every field, including Updated")
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
					ctrlMacs := protoNvosMacs(ctrlES)
					if len(ctrlMacs) != 0 || len(updated.NvosMacAddresses) != 0 {
						assert.Equal(t, ctrlMacs, updated.NvosMacAddresses,
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
					cesMacs := protoNvosMacs(ces)
					if len(cesMacs) != 0 || len(created.NvosMacAddresses) != 0 {
						assert.Equal(t, cesMacs, created.NvosMacAddresses,
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
	t.Run("API write after inventory read survives reconciliation", func(t *testing.T) {
		site := cwu.TestBuildSite(t, dbSession, ip, "concurrent-update-site", cdbm.SiteStatusRegistered, nil, ipu)
		row, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
			ExpectedSwitchID:   uuid.New(),
			SiteID:             site.ID,
			BmcMacAddress:      "00:11:22:33:99:01",
			SwitchSerialNumber: "SN-CONCURRENT",
			Name:               cutil.GetPtr("original-name"),
			Description:        cutil.GetPtr("original-description"),
			CreatedBy:          ipu.ID,
		})
		require.NoError(t, err)
		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedSwitch)(nil))

		// Commit the API edit after the inventory read has cached the old row.
		var newer *cdbm.ExpectedSwitch
		var writeErr error
		fired := false
		hook := &testExpectedSwitchAfterReadHook{afterRead: func() {
			fired = true
			newer, writeErr = esDAO.Update(ctx, nil, cdbm.ExpectedSwitchUpdateInput{
				ExpectedSwitchID: row.ID,
				Name:             cutil.GetPtr("API-name"),
				Description:      cutil.GetPtr("API-description"),
			})
		}}
		dbSession.DB.AddQueryHook(hook)
		defer func() { hook.afterRead = nil }()
		inventory := &corev1.ExpectedSwitchInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			ExpectedSwitches: []*corev1.ExpectedSwitch{{
				ExpectedSwitchId:   &corev1.UUID{Value: row.ID.String()},
				BmcMacAddress:      row.BmcMacAddress,
				SwitchSerialNumber: row.SwitchSerialNumber,
				Metadata:           &corev1.Metadata{Name: "inventory-name", Description: "inventory-description"},
			}},
		}
		mei := ManageExpectedSwitch{dbSession: dbSession}
		err = mei.UpdateExpectedSwitchesInDB(ctx, site.ID, inventory)
		require.True(t, fired)
		require.NoError(t, writeErr)
		require.NoError(t, err)
		stored, err := esDAO.Get(ctx, nil, row.ID, nil, false)
		require.NoError(t, err)
		assert.Equal(t, newer, stored, "both API values and Updated must survive")

		// A later inventory can still replace the supplied metadata.
		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedSwitch)(nil))
		err = mei.UpdateExpectedSwitchesInDB(ctx, site.ID, inventory)
		require.NoError(t, err)
		stored, err = esDAO.Get(ctx, nil, row.ID, nil, false)
		require.NoError(t, err)
		assert.Equal(t, cutil.GetPtr("inventory-name"), stored.Name)
		assert.Equal(t, cutil.GetPtr("inventory-description"), stored.Description)
	})

	t.Run("API write after inventory read survives deletion", func(t *testing.T) {
		site := cwu.TestBuildSite(t, dbSession, ip, "concurrent-delete-site", cdbm.SiteStatusRegistered, nil, ipu)
		row, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
			ExpectedSwitchID:   uuid.New(),
			SiteID:             site.ID,
			BmcMacAddress:      "00:11:22:33:99:03",
			SwitchSerialNumber: "SN-CONCURRENT-DELETE",
			Name:               cutil.GetPtr("original-name"),
			Description:        cutil.GetPtr("original-description"),
			CreatedBy:          ipu.ID,
		})
		require.NoError(t, err)
		companion, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
			ExpectedSwitchID:   uuid.New(),
			SiteID:             site.ID,
			BmcMacAddress:      "00:11:22:33:99:04",
			SwitchSerialNumber: "SN-UNCHANGED-DELETE",
			CreatedBy:          ipu.ID,
		})
		require.NoError(t, err)
		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedSwitch)(nil))
		beforeRead, err := esDAO.Get(ctx, nil, row.ID, nil, false)
		require.NoError(t, err)
		require.False(t, site.IsTimeWithinStaleInventoryThreshold(beforeRead.Updated))

		// Commit the API edit immediately before `DELETE` executes. An unlocked
		// version recheck followed by an unconditional delete would lose it.
		var newer *cdbm.ExpectedSwitch
		var writeErr error
		hook := &testExpectedSwitchBeforeDeleteHook{rowID: row.ID, beforeDelete: func() {
			newer, writeErr = esDAO.Update(ctx, nil, cdbm.ExpectedSwitchUpdateInput{
				ExpectedSwitchID: row.ID,
				Name:             cutil.GetPtr("API-name"),
				Description:      cutil.GetPtr("API-description"),
			})
		}}
		dbSession.DB.AddQueryHook(hook)
		defer func() { hook.beforeDelete = nil }()
		inventory := &corev1.ExpectedSwitchInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		}
		mei := ManageExpectedSwitch{dbSession: dbSession}
		err = mei.UpdateExpectedSwitchesInDB(ctx, site.ID, inventory)
		require.NoError(t, err)
		require.True(t, hook.fired)
		require.NoError(t, hook.err)
		require.NoError(t, writeErr)
		require.True(t, newer.Updated.After(beforeRead.Updated))
		stored, err := esDAO.Get(ctx, nil, row.ID, nil, false)
		require.NoError(t, err)
		assert.Equal(t, newer, stored, "both API values and Updated must survive")
		_, err = esDAO.Get(ctx, nil, companion.ID, nil, false)
		require.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})

	t.Run("delete failure allows other deletions and retries on later inventory", func(t *testing.T) {
		site := cwu.TestBuildSite(t, dbSession, ip, "failed-delete-site", cdbm.SiteStatusRegistered, nil, ipu)
		row, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
			ExpectedSwitchID:   uuid.New(),
			SiteID:             site.ID,
			BmcMacAddress:      "00:11:22:33:99:05",
			SwitchSerialNumber: "SN-FAILED-DELETE",
			CreatedBy:          ipu.ID,
		})
		require.NoError(t, err)
		companion, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
			ExpectedSwitchID:   uuid.New(),
			SiteID:             site.ID,
			BmcMacAddress:      "00:11:22:33:99:06",
			SwitchSerialNumber: "SN-CONTINUE-DELETE",
			CreatedBy:          ipu.ID,
		})
		require.NoError(t, err)
		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedSwitch)(nil))
		// `GetAll` orders by `created`, so process the failing row first.
		_, err = dbSession.DB.NewUpdate().Model((*cdbm.ExpectedSwitch)(nil)).
			Set("created = ?", companion.Created.Add(-time.Hour)).Where("id = ?", row.ID).Exec(ctx)
		require.NoError(t, err)
		rows, _, err := esDAO.GetAll(ctx, nil, cdbm.ExpectedSwitchFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{}, nil)
		require.NoError(t, err)
		require.Len(t, rows, 2)
		require.Equal(t, row.ID, rows[0].ID)
		require.Equal(t, companion.ID, rows[1].ID)

		_, err = dbSession.DB.ExecContext(ctx, `CREATE FUNCTION fail_expected_switch_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected database failure'; END $$`)
		require.NoError(t, err)
		defer func() {
			_, cleanupErr := dbSession.DB.ExecContext(ctx, "DROP FUNCTION fail_expected_switch_delete() CASCADE")
			require.NoError(t, cleanupErr)
		}()
		_, err = dbSession.DB.ExecContext(ctx, fmt.Sprintf("CREATE TRIGGER fail_delete BEFORE DELETE ON expected_switch FOR EACH ROW WHEN (OLD.id = '%s') EXECUTE FUNCTION fail_expected_switch_delete()", row.ID))
		require.NoError(t, err)
		deleted, err := esDAO.DeleteIfUnchanged(ctx, nil, row.ID, rows[0].Updated)
		require.False(t, deleted)
		require.ErrorContains(t, err, "injected database failure")
		hook := &testExpectedSwitchBeforeDeleteHook{rowID: row.ID}
		dbSession.DB.AddQueryHook(hook)
		inventory := &corev1.ExpectedSwitchInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}
		mei := ManageExpectedSwitch{dbSession: dbSession}
		require.NoError(t, mei.UpdateExpectedSwitchesInDB(ctx, site.ID, inventory))
		require.True(t, hook.fired)
		require.ErrorContains(t, hook.err, "injected database failure")
		stored, err := esDAO.Get(ctx, nil, row.ID, nil, false)
		require.NoError(t, err)
		assert.Equal(t, &rows[0], stored)
		_, err = esDAO.Get(ctx, nil, companion.ID, nil, false)
		require.ErrorIs(t, err, cdb.ErrDoesNotExist)

		// A later inventory retries the row once the database accepts writes.
		_, err = dbSession.DB.ExecContext(ctx, "DROP TRIGGER fail_delete ON expected_switch")
		require.NoError(t, err)
		require.NoError(t, mei.UpdateExpectedSwitchesInDB(ctx, site.ID, inventory))
		_, err = esDAO.Get(ctx, nil, row.ID, nil, false)
		require.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})

	t.Run("row lock protects reconciliation through commit", func(t *testing.T) {
		ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
		defer cancel()
		site := cwu.TestBuildSite(t, dbSession, ip, "locked-update-site", cdbm.SiteStatusRegistered, nil, ipu)
		row, err := esDAO.Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
			ExpectedSwitchID:   uuid.New(),
			SiteID:             site.ID,
			BmcMacAddress:      "00:11:22:33:99:02",
			SwitchSerialNumber: "SN-LOCKED",
			Name:               cutil.GetPtr("original-name"),
			Description:        cutil.GetPtr("original-description"),
			CreatedBy:          ipu.ID,
		})
		require.NoError(t, err)
		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedSwitch)(nil))

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
		hook := &testExpectedSwitchAfterReadHook{
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
		inventory := &corev1.ExpectedSwitchInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			ExpectedSwitches: []*corev1.ExpectedSwitch{{
				ExpectedSwitchId:   &corev1.UUID{Value: row.ID.String()},
				BmcMacAddress:      row.BmcMacAddress,
				SwitchSerialNumber: row.SwitchSerialNumber,
				Metadata:           &corev1.Metadata{Name: "inventory-name", Description: "inventory-description"},
			}},
		}
		mei := ManageExpectedSwitch{dbSession: dbSession}
		var activityErr, writerErr error
		workers.Go(func() {
			activityErr = mei.UpdateExpectedSwitchesInDB(context.WithValue(ctx, testExpectedSwitchReconcileContextKey{}, hook), site.ID, inventory)
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
				_, err = esDAO.Update(ctx, tx, cdbm.ExpectedSwitchUpdateInput{
					ExpectedSwitchID: row.ID,
					Name:             cutil.GetPtr("API-name"),
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
		stored, err := esDAO.Get(ctx, nil, row.ID, nil, false)
		require.NoError(t, err)
		assert.Equal(t, cutil.GetPtr("API-name"), stored.Name)
		assert.Equal(t, cutil.GetPtr("inventory-description"), stored.Description)
	})

	t.Run("sync supplied fields and preserve Cloud metadata", func(t *testing.T) {
		site := cwu.TestBuildSite(t, dbSession, ip, "snapshot-site", cdbm.SiteStatusRegistered, nil, ipu)
		id := uuid.New()
		mei := ManageExpectedSwitch{dbSession: dbSession}
		cases := []struct {
			name     string
			prepare  func(t *testing.T)
			reported *corev1.ExpectedSwitch
			want     cdbm.ExpectedSwitch
		}{
			{
				name: "create supplied Core fields",
				reported: &corev1.ExpectedSwitch{
					BmcIpAddress: "192.0.2.10",
					RackId:       &corev1.RackId{Id: "initial-rack"},
					Metadata: &corev1.Metadata{
						Name:        "initial-name",
						Description: "initial-description",
						Labels: []*corev1.Label{
							{Key: "model", Value: cutil.GetPtr("initial-model")},
						},
					},
					NvosInterfaces: []*corev1.ExpectedNvosInterface{
						{MacAddress: "aa:bb:cc:dd:ee:01", IsPrimary: true},
						{MacAddress: "aa:bb:cc:dd:ee:02"},
					},
				},
				want: cdbm.ExpectedSwitch{
					BmcIpAddress:          cutil.GetPtr("192.0.2.10"),
					RackID:                cutil.GetPtr("initial-rack"),
					Name:                  cutil.GetPtr("initial-name"),
					Description:           cutil.GetPtr("initial-description"),
					Labels:                cdbm.Labels{"model": "initial-model"},
					NvosMacAddresses:      []string{"aa:bb:cc:dd:ee:01", "aa:bb:cc:dd:ee:02"},
					NvosPrimaryMacAddress: cutil.GetPtr("aa:bb:cc:dd:ee:01"),
				},
			},
		}
		for _, field := range []struct {
			name  string
			apply func(*corev1.ExpectedSwitch, *cdbm.ExpectedSwitch)
		}{
			{"rack", func(reported *corev1.ExpectedSwitch, want *cdbm.ExpectedSwitch) {
				reported.RackId.Id = "updated-rack"
				want.RackID = cutil.GetPtr("updated-rack")
			}},
			{"name", func(reported *corev1.ExpectedSwitch, want *cdbm.ExpectedSwitch) {
				reported.Metadata.Name = "updated-name"
				reported.Metadata.Description = ""
				want.Name = cutil.GetPtr("updated-name")
			}},
			{"description", func(reported *corev1.ExpectedSwitch, want *cdbm.ExpectedSwitch) {
				reported.Metadata.Name = ""
				reported.Metadata.Description = "updated-description"
				want.Description = cutil.GetPtr("updated-description")
			}},
		} {
			next := cases[len(cases)-1]
			next.name = "replace only " + field.name
			next.reported = proto.Clone(next.reported).(*corev1.ExpectedSwitch)
			field.apply(next.reported, &next.want)
			cases = append(cases, next)
		}

		bmcIPUpdate := cases[len(cases)-1]
		bmcIPUpdate.name = "replace only the BMC IP address"
		bmcIPUpdate.reported = proto.Clone(cases[len(cases)-1].reported).(*corev1.ExpectedSwitch)
		bmcIPUpdate.reported.BmcIpAddress = "192.0.2.11"
		bmcIPUpdate.want.BmcIpAddress = cutil.GetPtr("192.0.2.11")
		cases = append(cases, bmcIPUpdate)

		primaryUpdate := cases[len(cases)-1]
		primaryUpdate.name = "replace only the NVOS primary"
		primaryUpdate.reported = proto.Clone(primaryUpdate.reported).(*corev1.ExpectedSwitch)
		primaryUpdate.reported.NvosInterfaces[0].IsPrimary = false
		primaryUpdate.reported.NvosInterfaces[1].IsPrimary = true
		primaryUpdate.want.NvosPrimaryMacAddress = cutil.GetPtr("aa:bb:cc:dd:ee:02")
		cases = append(cases, primaryUpdate)

		preserved := cases[len(cases)-1]
		preserved.name = "preserve Cloud metadata when Core omits it"
		preserved.reported = &corev1.ExpectedSwitch{Metadata: &corev1.Metadata{Labels: []*corev1.Label{
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
		preserved.want.NvosMacAddresses = []string{}
		// Core reporting no NVOS ports clears the primary selector too, rather
		// than leaving it pointing at a MAC that is no longer present. The
		// withoutLabels case below clones this want and inherits the cleared
		// selector.
		preserved.want.NvosPrimaryMacAddress = nil
		preserved.prepare = func(t *testing.T) {
			_, err := esDAO.Update(ctx, nil, cdbm.ExpectedSwitchUpdateInput{
				ExpectedSwitchID: id,
				Name:             preserved.want.Name,
				Description:      preserved.want.Description,
				RackID:           preserved.want.RackID,
				Manufacturer:     preserved.want.Manufacturer,
				Model:            preserved.want.Model,
				SlotID:           preserved.want.SlotID,
				TrayIdx:          preserved.want.TrayIdx,
				HostID:           preserved.want.HostID,
			})
			require.NoError(t, err)
		}
		cases = append(cases, preserved)

		withoutLabels := preserved
		withoutLabels.name = "clear omitted labels without clearing Cloud metadata"
		withoutLabels.prepare = nil
		withoutLabels.reported = &corev1.ExpectedSwitch{}
		withoutLabels.want.Labels = cdbm.Labels{}
		cases = append(cases, withoutLabels)

		var created time.Time
		createdBy := site.ID
		for i, tc := range cases {
			t.Run(tc.name, func(t *testing.T) {
				if i != 0 {
					_, err := esDAO.Get(ctx, nil, id, nil, false)
					require.NoError(t, err, "update snapshot requires the preceding create step")
				}
				if tc.prepare != nil {
					tc.prepare(t)
				}
				tc.reported.ExpectedSwitchId = &corev1.UUID{Value: id.String()}
				tc.reported.BmcMacAddress = "00:11:22:33:88:01"
				tc.reported.SwitchSerialNumber = "SN-SNAPSHOT"
				inventory := &corev1.ExpectedSwitchInventory{
					ExpectedSwitches: []*corev1.ExpectedSwitch{tc.reported},
					InventoryStatus:  corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				}
				cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedSwitch)(nil))
				err := mei.UpdateExpectedSwitchesInDB(ctx, site.ID, inventory)
				require.NoError(t, err)
				stored, err := esDAO.Get(ctx, nil, id, nil, false)
				require.NoError(t, err)
				if created.IsZero() {
					created = stored.Created
				}
				tc.want.ID = id
				tc.want.SiteID = site.ID
				tc.want.BmcMacAddress = tc.reported.BmcMacAddress
				tc.want.SwitchSerialNumber = tc.reported.SwitchSerialNumber
				tc.want.Created = created
				tc.want.CreatedBy = createdBy
				tc.want.Updated = stored.Updated
				assert.Equal(t, tc.want, *stored)

				// Repeating an aged snapshot must not refresh `Updated`.
				cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.ExpectedSwitch)(nil))
				beforeRepeat, err := esDAO.Get(ctx, nil, id, nil, false)
				require.NoError(t, err)
				err = mei.UpdateExpectedSwitchesInDB(ctx, site.ID, inventory)
				require.NoError(t, err)
				afterRepeat, err := esDAO.Get(ctx, nil, id, nil, false)
				require.NoError(t, err)
				assert.Equal(t, beforeRepeat.Updated, afterRepeat.Updated)

				// Subsequent snapshots must preserve the Cloud creator.
				createdBy = ipu.ID
				_, err = dbSession.DB.Exec("UPDATE expected_switch SET created_by = ? WHERE id = ?", createdBy, id)
				require.NoError(t, err)
			})
		}
	})

	t.Run("recent row survives omitted inventory", func(t *testing.T) {
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
	})
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
