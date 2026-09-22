// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrack

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

func TestManageExpectedRack_UpdateExpectedRacksInDB(t *testing.T) {
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
			Name:           fmt.Sprintf("Rack %d", i),
			Description:    fmt.Sprintf("Rack %d description", i),
			Labels:         labels,
			CreatedBy:      ipu.ID,
		})
		assert.NoError(t, cerr)

		// Update creation and update timestamp to be earlier than inventory processing interval so
		// reconciliation considers them outside the race-condition window.
		_, uerr := dbSession.DB.Exec("UPDATE expected_rack SET created = ?, updated = ? WHERE id = ?",
			time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval*2)),
			time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval*2)),
			er.ID.String())
		assert.NoError(t, uerr)

		pagedExpectedRacks = append(pagedExpectedRacks, er)
		pagedInvIds = append(pagedInvIds, er.RackID)
	}

	expectedRacksToUpdate := []*cdbm.ExpectedRack{}
	pagedCtrlExpectedRacks := []*corev1.ExpectedRack{}

	for i := 0; i < 14; i++ {
		ctrlExpectedRack := &corev1.ExpectedRack{
			RackId:        &corev1.RackId{Id: pagedExpectedRacks[i].RackID},
			RackProfileId: &corev1.RackProfileId{Id: pagedExpectedRacks[i].RackProfileID},
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
				ctrlExpectedRack.Metadata.Name = fmt.Sprintf("Updated Rack %d", i)
				ctrlExpectedRack.Metadata.Description = fmt.Sprintf("Updated Rack %d description", i)
				expectedRacksToUpdate = append(expectedRacksToUpdate, pagedExpectedRacks[i])
			}
		}

		// Test label updates
		if i == 1 {
			// Add labels to a rack that didn't have them before
			ctrlExpectedRack.Metadata.Labels = []*corev1.Label{
				{Key: "new-label", Value: cutil.GetPtr("new-value")},
			}
			expectedRacksToUpdate = append(expectedRacksToUpdate, pagedExpectedRacks[i])
		} else if i == 5 {
			// Modify existing labels
			ctrlExpectedRack.Metadata.Labels = []*corev1.Label{
				{Key: "region", Value: cutil.GetPtr(fmt.Sprintf("region-updated-%d", i/5))},
				{Key: "row", Value: cutil.GetPtr(fmt.Sprintf("row-%d", i))},
				{Key: "status", Value: cutil.GetPtr("active")},
			}
			expectedRacksToUpdate = append(expectedRacksToUpdate, pagedExpectedRacks[i])
		} else if i == 10 {
			// Remove labels (set to empty labels array)
			ctrlExpectedRack.Metadata.Labels = []*corev1.Label{}
			expectedRacksToUpdate = append(expectedRacksToUpdate, pagedExpectedRacks[i])
		}

		pagedCtrlExpectedRacks = append(pagedCtrlExpectedRacks, ctrlExpectedRack)
	}

	expectedRacksToDelete := pagedExpectedRacks[14:18]

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
						ItemIds:     pagedInvIds[0:14],
					},
				},
			},
			expectedRacksToUpdate: expectedRacksToUpdate,
			expectedRacksToDelete: []*cdbm.ExpectedRack{},
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
			expectedRacksToUpdate: []*cdbm.ExpectedRack{},
			expectedRacksToDelete: expectedRacksToDelete,
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

			for _, er := range tt.expectedRacksToUpdate {
				updated := racksByRackID[er.RackID]
				assert.NotNil(t, updated, fmt.Sprintf("ExpectedRack %v should exist", er.RackID))
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
				if ctrlER != nil && updated != nil {
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
			}

			// Verify deletions
			for _, er := range tt.expectedRacksToDelete {
				deleted := racksByRackID[er.RackID]
				assert.Nil(t, deleted, fmt.Sprintf("ExpectedRack %v should have been deleted", er.RackID))
			}

			// Verify newly created racks have correct fields/labels and that nil/empty-id entries
			// were ignored.
			for _, cer := range tt.args.expectedRackInventory.ExpectedRacks {
				if cer == nil || cer.RackId == nil || cer.RackId.Id == "" {
					continue
				}
				created := racksByRackID[cer.RackId.Id]
				if created != nil {
					reportedName := ""
					reportedDescription := ""
					if cer.Metadata != nil {
						reportedName = cer.Metadata.Name
						reportedDescription = cer.Metadata.Description
					}
					assert.Equal(t, reportedName, created.Name,
						fmt.Sprintf("ExpectedRack %v Name should match on creation", cer.RackId.Id))
					assert.Equal(t, reportedDescription, created.Description,
						fmt.Sprintf("ExpectedRack %v Description should match on creation", cer.RackId.Id))

					var expectedLabels cdbm.Labels
					expectedLabels.FromProto(cer.Metadata.GetLabels())
					if len(expectedLabels) == 0 && len(created.Labels) == 0 {
						// Both effectively empty, which is correct
					} else {
						assert.Equal(t, expectedLabels, created.Labels,
							fmt.Sprintf("ExpectedRack %v labels should match on creation", cer.RackId.Id))
					}
				}
			}
		})
	}
}

func TestManageExpectedRack_UpdateExpectedRacksInDB_NoChange(t *testing.T) {
	// Verify that when the inventory matches the DB state exactly, no update is performed.
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
		Labels:         map[string]string{"foo": "bar"},
		CreatedBy:      ipu.ID,
	})
	assert.NoError(t, err)

	// Push the row's Updated timestamp into the past so we can detect whether the activity
	// touched it (an Update would refresh Updated to current time).
	pastTime := time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval * 2))
	_, uerr := dbSession.DB.Exec("UPDATE expected_rack SET created = ?, updated = ? WHERE id = ?",
		pastTime, pastTime, er.ID.String())
	assert.NoError(t, uerr)

	tSiteClientPool := testTemporalSiteClientPool(t)
	mer := ManageExpectedRack{
		dbSession:      dbSession,
		siteClientPool: tSiteClientPool,
	}

	// Build inventory that exactly mirrors the DB record
	inventory := &corev1.ExpectedRackInventory{
		ExpectedRacks: []*corev1.ExpectedRack{
			{
				RackId:        &corev1.RackId{Id: er.RackID},
				RackProfileId: &corev1.RackProfileId{Id: er.RackProfileID},
				Metadata: &corev1.Metadata{
					Name:        er.Name,
					Description: er.Description,
					Labels: []*corev1.Label{
						{Key: "foo", Value: cutil.GetPtr("bar")},
					},
				},
			},
		},
		Timestamp:       timestamppb.Now(),
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
	}

	err = mer.UpdateExpectedRacksInDB(ctx, st.ID, inventory)
	assert.NoError(t, err)

	// Re-read and verify Updated wasn't refreshed (the row was not modified by the activity)
	got, gerr := erDAO.Get(ctx, nil, er.ID, nil, false)
	assert.NoError(t, gerr)
	// allow 1s of slop for timestamp comparison
	assert.WithinDuration(t, pastTime, got.Updated, time.Second,
		"row should not have been updated when nothing changed")
}

func TestManageExpectedRack_UpdateExpectedRacksInDB_RaceCondition(t *testing.T) {
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
