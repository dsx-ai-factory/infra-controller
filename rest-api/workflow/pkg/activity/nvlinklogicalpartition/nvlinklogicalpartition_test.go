// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvlinklogicalpartition

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
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	tmocks "go.temporal.io/sdk/mocks"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
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

func testNVLinkLogicalPartitionInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testNVLinkLogicalPartitionSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	// create TenantSite table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantSite)(nil))
	assert.Nil(t, err)
	// create NVLinkLogicalPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkLogicalPartition)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
}

func TestManageNVLinkLogicalPartition_UpdateNVLinkLogicalPartitionsInDB(t *testing.T) {
	ctx := context.Background()
	dbSession := testNVLinkLogicalPartitionInitDB(t)
	defer dbSession.Close()

	testNVLinkLogicalPartitionSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg}, tnRoles)

	tn := cwu.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, nil, tnu)
	assert.NotNil(t, tn)

	st1 := cwu.TestBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, nil, ipu)
	st2 := cwu.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)

	ts1 := cwu.TestBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st1.ID, tn.ID)
	assert.NotNil(t, ts1)

	nvllp1 := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-1", cwutil.GetPtr("Test description"), st1, tn, cdbm.NVLinkLogicalPartitionStatusPending, false)
	assert.NotNil(t, nvllp1)

	// Set updated earlier than the inventory receipt interval (with buffer to exceed threshold)
	_, err := dbSession.DB.Exec("UPDATE nvlink_logical_partition SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), nvllp1.ID.String())
	assert.NoError(t, err)

	nvllp2 := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-2", nil, st1, tn, cdbm.NVLinkLogicalPartitionStatusProvisioning, false)
	assert.NotNil(t, nvllp2)

	// Set created earlier than the inventory receipt interval (with buffer to exceed threshold)
	_, err = dbSession.DB.Exec("UPDATE nvlink_logical_partition SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), nvllp2.ID.String())
	assert.NoError(t, err)

	nvllp3 := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-3", nil, st1, tn, cdbm.NVLinkLogicalPartitionStatusDeleting, false)
	assert.NotNil(t, nvllp3)

	// Set created earlier than the inventory receipt interval (with buffer to exceed threshold)
	_, err = dbSession.DB.Exec("UPDATE nvlink_logical_partition SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), nvllp3.ID.String())
	assert.NoError(t, err)

	nvllp4 := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-4", nil, st1, tn, cdbm.NVLinkLogicalPartitionStatusDeleting, false)
	assert.NotNil(t, nvllp4)

	// Set created earlier than the inventory receipt interval (with buffer to exceed threshold)
	_, err = dbSession.DB.Exec("UPDATE nvlink_logical_partition SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), nvllp4.ID.String())
	assert.NoError(t, err)

	nvllp5 := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-5", nil, st1, tn, cdbm.NVLinkLogicalPartitionStatusDeleting, false)
	assert.NotNil(t, nvllp5)

	// Set created earlier than the inventory receipt interval (with buffer to exceed threshold)
	_, err = dbSession.DB.Exec("UPDATE nvlink_logical_partition SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), nvllp5.ID.String())
	assert.NoError(t, err)

	nvllp6 := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-6", nil, st1, tn, cdbm.NVLinkLogicalPartitionStatusDeleting, false)
	assert.NotNil(t, nvllp6)

	// Set created earlier than the inventory receipt interval (with buffer to exceed threshold)
	_, err = dbSession.DB.Exec("UPDATE nvlink_logical_partition SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), nvllp6.ID.String())
	assert.NoError(t, err)

	nvllp7 := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-7", nil, st1, tn, cdbm.NVLinkLogicalPartitionStatusReady, false)
	assert.NotNil(t, nvllp7)

	// Set created and updated earlier than the inventory receipt interval (with buffer to exceed threshold)
	_, err = dbSession.DB.Exec("UPDATE nvlink_logical_partition SET created = ?, updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), nvllp7.ID.String())
	assert.NoError(t, err)

	nvllp8 := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-8", nil, st1, tn, cdbm.NVLinkLogicalPartitionStatusError, true)
	assert.NotNil(t, nvllp8)

	nvllp9 := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-9", nil, st1, tn, cdbm.NVLinkLogicalPartitionStatusProvisioning, false)
	assert.NotNil(t, nvllp9)

	// Set created earlier than the inventory receipt interval (with buffer to exceed threshold)
	_, err = dbSession.DB.Exec("UPDATE nvlink_logical_partition SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), nvllp9.ID.String())
	assert.NoError(t, err)

	nvllp10 := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-10", nil, st1, tn, cdbm.NVLinkLogicalPartitionStatusDeleting, false)
	assert.NotNil(t, nvllp10)

	// Set created earlier than the inventory receipt interval (with buffer to exceed threshold)
	_, err = dbSession.DB.Exec("UPDATE nvlink_logical_partition SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), nvllp10.ID.String())
	assert.NoError(t, err)

	nvllp11 := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-11", nil, st1, tn, cdbm.NVLinkLogicalPartitionStatusReady, false)
	assert.NotNil(t, nvllp11)

	// Set created and updated earlier than the inventory receipt interval (with buffer to exceed threshold)
	_, err = dbSession.DB.Exec("UPDATE nvlink_logical_partition SET created = ?, updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), nvllp11.ID.String())
	assert.NoError(t, err)

	// Build InfiniBand Partition inventory that is paginated
	// Generate data for 34 NVLinkLogicalPartitions reported from Site Agent while Cloud has 38 NVLinkLogicalPartitions
	pagedIbps := []*cdbm.NVLinkLogicalPartition{}
	pagedInvIds := []string{}
	for i := 0; i < 38; i++ {
		nvllp := cwu.TestBuildNVLinkLogicalPartition(t, dbSession, fmt.Sprintf("test-vpc-paged-%d", i), nil, st2, tn, cdbm.NVLinkLogicalPartitionStatusReady, false)
		// Update created and updated timestamps to be earlier than inventory processing interval (with buffer to exceed threshold)
		_, err = dbSession.DB.Exec("UPDATE nvlink_logical_partition SET created = ?, updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*3), time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*3), nvllp.ID.String())
		assert.NoError(t, err)
		pagedIbps = append(pagedIbps, nvllp)
		pagedInvIds = append(pagedInvIds, nvllp.ID.String())
	}

	pagedCtrlNvllps := []*corev1.NVLinkLogicalPartition{}
	for i := 0; i < 34; i++ {
		ctrlNvllp := &corev1.NVLinkLogicalPartition{
			Id: &corev1.NVLinkLogicalPartitionId{Value: pagedIbps[i].ID.String()},
			Config: &corev1.NVLinkLogicalPartitionConfig{
				Metadata: &corev1.Metadata{
					Name: pagedIbps[i].ID.String(),
				},
			},
		}
		pagedCtrlNvllps = append(pagedCtrlNvllps, ctrlNvllp)
	}

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
		ctx                             context.Context
		siteID                          uuid.UUID
		nvLinkLogicalPartitionInventory *corev1.NVLinkLogicalPartitionInventory
	}

	tests := []struct {
		name                            string
		fields                          fields
		args                            args
		updatedNVLinkLogicalPartitions  []*cdbm.NVLinkLogicalPartition
		readyNVLinkLogicalPartitions    []*cdbm.NVLinkLogicalPartition
		deletedNVLinkLogicalPartitions  []*cdbm.NVLinkLogicalPartition
		missingNVLinkLogicalPartitions  []*cdbm.NVLinkLogicalPartition
		deletingNVLinkLogicalPartitions []*cdbm.NVLinkLogicalPartition
		restoredNVLinkLogicalPartition  *cdbm.NVLinkLogicalPartition

		wantErr bool
	}{
		{
			name: "test NVLinkLogicalPartition inventory processing error, non-existent Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				nvLinkLogicalPartitionInventory: &corev1.NVLinkLogicalPartitionInventory{
					Partitions: []*corev1.NVLinkLogicalPartition{},
				},
			},
			wantErr: true,
		},
		{
			name: "test NVLinkLogicalPartition inventory processing success",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st1.ID,
				nvLinkLogicalPartitionInventory: &corev1.NVLinkLogicalPartitionInventory{
					Partitions: []*corev1.NVLinkLogicalPartition{
						{
							Id: &corev1.NVLinkLogicalPartitionId{Value: nvllp1.ID.String()},
							Config: &corev1.NVLinkLogicalPartitionConfig{
								Metadata: &corev1.Metadata{
									Name:        nvllp1.ID.String(),
									Description: "Test description updated",
								},
							},
							Status: &corev1.NVLinkLogicalPartitionStatus{
								State: corev1.TenantState_PROVISIONING,
							},
						},
						{
							Id: &corev1.NVLinkLogicalPartitionId{Value: nvllp2.ID.String()},
							Config: &corev1.NVLinkLogicalPartitionConfig{
								Metadata: &corev1.Metadata{
									Name: nvllp2.ID.String(),
								},
							},
						},
						{
							Id: &corev1.NVLinkLogicalPartitionId{Value: nvllp3.ID.String()},
							Config: &corev1.NVLinkLogicalPartitionConfig{
								Metadata: &corev1.Metadata{
									Name:        nvllp3.ID.String(),
									Description: "Test description updated",
								},
							},
							Status: &corev1.NVLinkLogicalPartitionStatus{
								State: corev1.TenantState_PROVISIONING,
							},
						},
						{
							Id: &corev1.NVLinkLogicalPartitionId{Value: nvllp8.ID.String()},
							Config: &corev1.NVLinkLogicalPartitionConfig{
								Metadata: &corev1.Metadata{
									Name: nvllp8.ID.String(),
								},
							},
							Status: &corev1.NVLinkLogicalPartitionStatus{
								State: corev1.TenantState_READY,
							},
						},
						{
							Id: &corev1.NVLinkLogicalPartitionId{Value: uuid.NewString()},
							Config: &corev1.NVLinkLogicalPartitionConfig{
								Metadata: &corev1.Metadata{
									Name: nvllp9.ID.String(),
								},
							},
							Status: &corev1.NVLinkLogicalPartitionStatus{
								State: corev1.TenantState_READY,
							},
						},
						{
							Id: &corev1.NVLinkLogicalPartitionId{Value: uuid.NewString()},
							Config: &corev1.NVLinkLogicalPartitionConfig{
								Metadata: &corev1.Metadata{
									Name: nvllp10.ID.String(),
								},
							},
							Status: &corev1.NVLinkLogicalPartitionStatus{
								State: corev1.TenantState_READY,
							},
						},
					},
				},
			},
			updatedNVLinkLogicalPartitions:  []*cdbm.NVLinkLogicalPartition{nvllp1},
			deletedNVLinkLogicalPartitions:  []*cdbm.NVLinkLogicalPartition{nvllp5, nvllp6},
			missingNVLinkLogicalPartitions:  []*cdbm.NVLinkLogicalPartition{nvllp7, nvllp11},
			deletingNVLinkLogicalPartitions: []*cdbm.NVLinkLogicalPartition{nvllp3},
			restoredNVLinkLogicalPartition:  nvllp8,
			wantErr:                         false,
		},
		{
			name: "test paged NVLinkLogicalPartition inventory processing, empty inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st1.ID,
				nvLinkLogicalPartitionInventory: &corev1.NVLinkLogicalPartitionInventory{
					Partitions:      []*corev1.NVLinkLogicalPartition{},
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
			name: "test paged NVLinkLogicalPartition inventory processing, first page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				nvLinkLogicalPartitionInventory: &corev1.NVLinkLogicalPartitionInventory{
					Partitions: pagedCtrlNvllps[0:10],
					Timestamp:  timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			readyNVLinkLogicalPartitions: pagedIbps[0:34],
		},
		{
			name: "test paged NVLinkLogicalPartition inventory processing, last page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				nvLinkLogicalPartitionInventory: &corev1.NVLinkLogicalPartitionInventory{
					Partitions: pagedCtrlNvllps[30:34],
					Timestamp:  timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 4,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			readyNVLinkLogicalPartitions:   pagedIbps[0:34],
			missingNVLinkLogicalPartitions: pagedIbps[34:38],
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mnvllp := ManageNVLinkLogicalPartition{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tSiteClientPool,
			}

			// Mock the "UpdateNVLinkLogicalPartitionsInDB" activity
			mtc := &tmocks.Client{}
			mnvllp.siteClientPool.IDClientMap[st1.ID.String()] = mtc

			cwu.TestInventoryAgeUpdatedTimestamp(tt.args.ctx, t, dbSession, (*cdbm.NVLinkLogicalPartition)(nil))

			err := mnvllp.UpdateNVLinkLogicalPartitionsInDB(tt.args.ctx, tt.args.siteID, tt.args.nvLinkLogicalPartitionInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(dbSession)
			// Check that NVLinkLogicalPartition status was updated in DB for NVLinkLogicalPartition1
			for _, nvllp := range tt.updatedNVLinkLogicalPartitions {
				unvllp, _ := nvllpDAO.GetByID(ctx, nil, nvllp.ID, nil)
				assert.Equal(t, cdbm.NVLinkLogicalPartitionStatusProvisioning, unvllp.Status)
				assert.Equal(t, tt.args.nvLinkLogicalPartitionInventory.Partitions[0].Config.Metadata.Description, *unvllp.Description)
			}

			for _, nvllp := range tt.readyNVLinkLogicalPartitions {
				unvllp, _ := nvllpDAO.GetByID(ctx, nil, nvllp.ID, nil)

				assert.False(t, unvllp.IsMissingOnSite)
				assert.Equal(t, cdbm.NVLinkLogicalPartitionStatusReady, unvllp.Status)
			}

			for _, nvllp := range tt.deletedNVLinkLogicalPartitions {
				_, err = nvllpDAO.GetByID(ctx, nil, nvllp.ID, nil)
				require.Equal(t, cdb.ErrDoesNotExist, err, fmt.Sprintf("NVLink Logical Partition %s should have been deleted", nvllp.Name))
			}

			for _, nvllp := range tt.missingNVLinkLogicalPartitions {
				uv, _ := nvllpDAO.GetByID(ctx, nil, nvllp.ID, nil)
				assert.True(t, uv.IsMissingOnSite)
				assert.Equal(t, cdbm.NVLinkLogicalPartitionStatusError, uv.Status)
			}

			for _, nvllp := range tt.deletingNVLinkLogicalPartitions {
				uv, _ := nvllpDAO.GetByID(ctx, nil, nvllp.ID, nil)
				assert.Equal(t, cdbm.NVLinkLogicalPartitionStatusDeleting, uv.Status, "Deleting status should not be overwritten by inventory")
			}

			if tt.restoredNVLinkLogicalPartition != nil {
				rv, _ := nvllpDAO.GetByID(ctx, nil, tt.restoredNVLinkLogicalPartition.ID, nil)
				assert.False(t, rv.IsMissingOnSite)
				assert.Equal(t, cdbm.NVLinkLogicalPartitionStatusReady, rv.Status)
			}
		})
	}
}

func TestNewManageNVLinkLogicalPartition(t *testing.T) {
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
		want ManageNVLinkLogicalPartition
	}{
		{
			name: "test new ManageNVLinkLogicalPartition instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageNVLinkLogicalPartition{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewManageNVLinkLogicalPartition(tt.args.dbSession, tt.args.siteClientPool)
			assert.Equal(t, tt.want.dbSession, got.dbSession, "dbSession should match")
			assert.Equal(t, tt.want.siteClientPool, got.siteClientPool, "siteClientPool should match")
		})
	}
}
