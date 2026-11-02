// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package infinibandpartition

import (
	"context"
	"fmt"
	"reflect"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"

	"os"

	tmocks "go.temporal.io/sdk/mocks"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"go.temporal.io/sdk/testsuite"
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

func testInfiniBandPartitionInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testInfiniBandPartitionSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	// create Status Details table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
	// create InfiniBandPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InfiniBandPartition)(nil))
	assert.Nil(t, err)
	// create InfiniBandInterface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InfiniBandInterface)(nil))
	assert.Nil(t, err)
}

func TestManageInfiniBandPartition_UpdateInfiniBandPartitionsInDB(t *testing.T) {
	ctx := context.Background()
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg}, tnRoles)

	tn := util.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, nil, tnu)
	assert.NotNil(t, tn)

	st1 := util.TestBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, nil, ipu)
	st2 := util.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)
	st3 := util.TestBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, nil, ipu)
	st4 := util.TestBuildSite(t, dbSession, ip, "test-site-4", cdbm.SiteStatusRegistered, nil, ipu)
	st5 := util.TestBuildSite(t, dbSession, ip, "test-site-5", cdbm.SiteStatusRegistered, nil, ipu)
	st6 := util.TestBuildSite(t, dbSession, ip, "test-site-6", cdbm.SiteStatusRegistered, nil, ipu)

	ts1 := util.TestBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st1.ID, tn.ID)
	assert.NotNil(t, ts1)
	util.TestBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st3.ID, tnu.ID)
	util.TestBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st4.ID, tnu.ID)
	util.TestBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st5.ID, tnu.ID)
	util.TestBuildTenantSiteAssociation(t, dbSession, tnOrg, tn.ID, st6.ID, tnu.ID)
	ibp1 := util.TestBuildInfiniBandPartition(t, dbSession, "test-ibp-1", st1, tn, nil, cdbm.InfiniBandPartitionStatusPending, false)
	assert.NotNil(t, ibp1)

	ibp2 := util.TestBuildInfiniBandPartition(t, dbSession, "test-ibp-2", st1, tn, nil, cdbm.InfiniBandPartitionStatusProvisioning, false)
	assert.NotNil(t, ibp2)

	ibp3 := util.TestBuildInfiniBandPartition(t, dbSession, "test-ibp-3", st1, tn, nil, cdbm.InfiniBandPartitionStatusDeleting, false)
	assert.NotNil(t, ibp3)

	ibp4 := util.TestBuildInfiniBandPartition(t, dbSession, "test-ibp-4", st1, tn, cutil.GetPtr(uuid.New()), cdbm.InfiniBandPartitionStatusDeleting, false)
	assert.NotNil(t, ibp4)

	ibp5 := util.TestBuildInfiniBandPartition(t, dbSession, "test-ibp-5", st1, tn, cutil.GetPtr(uuid.New()), cdbm.InfiniBandPartitionStatusDeleting, false)
	assert.NotNil(t, ibp5)

	ibp6 := util.TestBuildInfiniBandPartition(t, dbSession, "test-ibp-6", st1, tn, nil, cdbm.InfiniBandPartitionStatusDeleting, false)
	assert.NotNil(t, ibp6)

	ibp7 := util.TestBuildInfiniBandPartition(t, dbSession, "test-ibp-7", st1, tn, cutil.GetPtr(uuid.New()), cdbm.InfiniBandPartitionStatusReady, false)
	assert.NotNil(t, ibp7)
	// Set created earlier than the inventory receipt interval
	_, err := dbSession.DB.Exec("UPDATE infiniband_partition SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), ibp7.ID.String())
	assert.NoError(t, err)

	ibp8 := util.TestBuildInfiniBandPartition(t, dbSession, "test-ibp-8", st1, tn, cutil.GetPtr(uuid.New()), cdbm.InfiniBandPartitionStatusError, true)
	assert.NotNil(t, ibp8)

	ibp9 := util.TestBuildInfiniBandPartition(t, dbSession, "test-ibp-9", st1, tn, cutil.GetPtr(uuid.New()), cdbm.InfiniBandPartitionStatusProvisioning, false)
	assert.NotNil(t, ibp9)

	ibp10 := util.TestBuildInfiniBandPartition(t, dbSession, "test-ibp-10", st1, tn, cutil.GetPtr(uuid.New()), cdbm.InfiniBandPartitionStatusDeleting, false)
	assert.NotNil(t, ibp10)

	ibp11 := util.TestBuildInfiniBandPartition(t, dbSession, "test-ibp-11", st1, tn, cutil.GetPtr(uuid.New()), cdbm.InfiniBandPartitionStatusReady, false)
	assert.NotNil(t, ibp11)
	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE infiniband_partition SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), ibp11.ID.String())
	assert.NoError(t, err)

	recoveredIbpID := uuid.New()
	failedRecoveredIbpID := uuid.New()
	terminalIbpID := uuid.New()
	recoveredDescription := "recovered from Site inventory"
	recoveredLabelValue := "recovered"

	util.TestBuildInfiniBandPartition(
		t,
		dbSession,
		"site-only-ibp",
		st3,
		tn,
		nil,
		cdbm.InfiniBandPartitionStatusReady,
		false,
	)

	softDeletedIbp := util.TestBuildInfiniBandPartition(
		t,
		dbSession,
		"test-ibp-soft-deleted",
		st4,
		tn,
		nil,
		cdbm.InfiniBandPartitionStatusDeleting,
		true,
	)
	restoredControllerID := softDeletedIbp.ID
	oldPartitionKey := "0x111"
	oldPartitionName := "old-partition"
	oldServiceLevel := 1
	oldRateLimit := float32(2)
	oldMtu := 2048
	oldSharp := true
	ibpDAO := cdbm.NewInfiniBandPartitionDAO(dbSession)
	_, err = ibpDAO.Update(ctx, nil, cdbm.InfiniBandPartitionUpdateInput{
		InfiniBandPartitionID: softDeletedIbp.ID,
		PartitionKey:          &oldPartitionKey,
		PartitionName:         &oldPartitionName,
		ServiceLevel:          &oldServiceLevel,
		RateLimit:             &oldRateLimit,
		Mtu:                   &oldMtu,
		EnableSharp:           &oldSharp,
		Labels:                map[string]string{"old": "label"},
	})
	require.NoError(t, err)
	err = ibpDAO.Delete(ctx, nil, softDeletedIbp.ID)
	require.NoError(t, err)
	_, err = dbSession.DB.Exec(
		"UPDATE infiniband_partition SET deleted = ? WHERE id = ?",
		time.Now().Add(-cutil.DefaultInventoryReceiptInterval*2),
		softDeletedIbp.ID,
	)
	require.NoError(t, err)

	// Build InfiniBand Partition inventory that is paginated
	// Generate data for 34 InfiniBand Partitions reported from Site Agent while Cloud has 38 InfiniBand Partitions
	pagedIbps := []*cdbm.InfiniBandPartition{}
	pagedInvIds := []string{}
	for i := 0; i < 38; i++ {
		ibp := util.TestBuildInfiniBandPartition(t, dbSession, fmt.Sprintf("test-vpc-paged-%d", i), st2, tn, cutil.GetPtr(uuid.New()), cdbm.InfiniBandPartitionStatusReady, false)
		// Update creation timestamp to be earlier than inventory processing interval
		_, err = dbSession.DB.Exec("UPDATE infiniband_partition SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), ibp.ID.String())
		assert.NoError(t, err)
		pagedIbps = append(pagedIbps, ibp)
		pagedInvIds = append(pagedInvIds, ibp.ID.String())
	}

	pagedCtrlIbps := []*corev1.IBPartition{}
	for i := 0; i < 34; i++ {
		ctrlIbp := &corev1.IBPartition{
			Id: &corev1.IBPartitionId{Value: pagedIbps[i].ID.String()},
			Config: &corev1.IBPartitionConfig{
				Name: pagedIbps[i].ID.String(),
			},
		}
		pagedCtrlIbps = append(pagedCtrlIbps, ctrlIbp)
	}

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	serviceLevel := int32(10)
	rateLimit := int32(100)
	mtu := int32(1500)

	type fields struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
		env            *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx                          context.Context
		siteID                       uuid.UUID
		infiniBandPartitionInventory *corev1.InfiniBandPartitionInventory
	}

	tests := []struct {
		name                         string
		fields                       fields
		args                         args
		updatedInfiniBandPartition   *cdbm.InfiniBandPartition
		readyInfiniBandPartitions    []*cdbm.InfiniBandPartition
		deletedInfiniBandPartitions  []*cdbm.InfiniBandPartition
		missingInfiniBandPartitions  []*cdbm.InfiniBandPartition
		restoredInfiniBandPartition  *cdbm.InfiniBandPartition
		unpairedInfiniBandPartitions []*cdbm.InfiniBandPartition

		wantErr bool
		check   func(t *testing.T)
	}{
		{
			name: "test InfiniBandPartition inventory processing error, non-existent Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				infiniBandPartitionInventory: &corev1.InfiniBandPartitionInventory{
					IbPartitions: []*corev1.IBPartition{},
				},
			},
			wantErr: true,
		},
		{
			name: "test InfiniBandPartition inventory processing success",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st1.ID,
				infiniBandPartitionInventory: &corev1.InfiniBandPartitionInventory{
					IbPartitions: []*corev1.IBPartition{
						{
							Id: &corev1.IBPartitionId{Value: ibp1.ID.String()},
							Config: &corev1.IBPartitionConfig{
								Name: ibp1.ID.String(),
							},
							Status: &corev1.IBPartitionStatus{
								State:        corev1.TenantState_PROVISIONING,
								Pkey:         cutil.GetPtr("106"),
								Partition:    cutil.GetPtr("test-ibp-1"),
								ServiceLevel: &serviceLevel,
								RateLimit:    &rateLimit,
								Mtu:          &mtu,
								EnableSharp:  cutil.GetPtr(false),
							},
						},
						{
							Id: &corev1.IBPartitionId{Value: ibp2.ID.String()},
							Config: &corev1.IBPartitionConfig{
								Name: ibp2.ID.String(),
							},
						},
						{
							Id: &corev1.IBPartitionId{Value: ibp3.ID.String()},
							Config: &corev1.IBPartitionConfig{
								Name: ibp3.ID.String(),
							},
						},
						{
							Id: &corev1.IBPartitionId{Value: ibp4.ID.String()},
							Config: &corev1.IBPartitionConfig{
								Name: ibp4.ID.String(),
							},
						},
						{
							Id: &corev1.IBPartitionId{Value: ibp8.ID.String()},
							Config: &corev1.IBPartitionConfig{
								Name: ibp8.ID.String(),
							},
							Status: &corev1.IBPartitionStatus{
								State: corev1.TenantState_READY,
							},
						},
						{
							Id: &corev1.IBPartitionId{Value: ibp9.ID.String()},
							Config: &corev1.IBPartitionConfig{
								Name: ibp9.ID.String(),
							},
							Status: &corev1.IBPartitionStatus{
								State: corev1.TenantState_READY,
							},
						},
						{
							Id: &corev1.IBPartitionId{Value: ibp10.ID.String()},
							Config: &corev1.IBPartitionConfig{
								Name: ibp10.ID.String(),
							},
							Status: &corev1.IBPartitionStatus{
								State: corev1.TenantState_READY,
							},
						},
					},
				},
			},
			updatedInfiniBandPartition:   ibp1,
			deletedInfiniBandPartitions:  []*cdbm.InfiniBandPartition{ibp5, ibp6},
			missingInfiniBandPartitions:  []*cdbm.InfiniBandPartition{ibp7, ibp11},
			restoredInfiniBandPartition:  ibp8,
			unpairedInfiniBandPartitions: []*cdbm.InfiniBandPartition{ibp9, ibp10},
			wantErr:                      false,
		},
		{
			name: "test InfiniBand Partition inventory auto-creates partition found only on Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				infiniBandPartitionInventory: &corev1.InfiniBandPartitionInventory{
					IbPartitions: []*corev1.IBPartition{
						{
							Id: &corev1.IBPartitionId{Value: recoveredIbpID.String()},
							Config: &corev1.IBPartitionConfig{
								Name:                 "legacy-recovered-name",
								TenantOrganizationId: tn.Org,
								Pkey:                 cutil.GetPtr("0x222"),
							},
							Metadata: &corev1.Metadata{
								Name:        "site-only-ibp",
								Description: recoveredDescription,
								Labels: []*corev1.Label{
									{Key: "source", Value: &recoveredLabelValue},
								},
							},
							Status: &corev1.IBPartitionStatus{
								State:        corev1.TenantState_READY,
								Pkey:         cutil.GetPtr("0x222"),
								Partition:    cutil.GetPtr("site-only-partition"),
								ServiceLevel: &serviceLevel,
								RateLimit:    &rateLimit,
								Mtu:          &mtu,
								EnableSharp:  cutil.GetPtr(true),
							},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			check: func(t *testing.T) {
				t.Helper()

				recovered, rerr := cdbm.NewInfiniBandPartitionDAO(dbSession).GetByID(ctx, nil, recoveredIbpID, nil)
				require.NoError(t, rerr)

				if !assert.NotNil(t, recovered) {
					return
				}

				assert.Equal(t, recoveredIbpID, recovered.ID)
				require.NotNil(t, recovered.ControllerIBPartitionID)
				assert.Equal(t, recoveredIbpID, *recovered.ControllerIBPartitionID)
				assert.Equal(t, "site-only-ibp-recovered-"+recoveredIbpID.String()[:8], recovered.Name)
				require.NotNil(t, recovered.Description)
				assert.Equal(t, recoveredDescription, *recovered.Description)
				assert.Equal(t, tn.Org, recovered.Org)
				assert.Equal(t, st3.ID, recovered.SiteID)
				assert.Equal(t, tn.ID, recovered.TenantID)
				assert.Equal(t, tn.CreatedBy, recovered.CreatedBy)
				assert.Equal(t, cdbm.InfiniBandPartitionStatusReady, recovered.Status)
				assert.Equal(t, map[string]string{"source": recoveredLabelValue}, map[string]string(recovered.Labels))
				require.NotNil(t, recovered.PartitionKey)
				assert.Equal(t, "0x222", *recovered.PartitionKey)
				require.NotNil(t, recovered.PartitionName)
				assert.Equal(t, "site-only-partition", *recovered.PartitionName)

				statusDetails, total, rerr := cdbm.NewStatusDetailDAO(dbSession).GetAll(
					ctx,
					nil,
					cdbm.StatusDetailFilterInput{EntityIDs: []string{recovered.ID.String()}},
					cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
				)
				require.NoError(t, rerr)
				assert.Equal(t, 1, total)

				if assert.Len(t, statusDetails, 1) {
					assert.Equal(t, string(cdbm.InfiniBandPartitionStatusReady), statusDetails[0].Status)
					assert.Equal(t, cutil.GetPtr("InfiniBand Partition was found on Site, ready for use"), statusDetails[0].Message)
				}
			},
		},
		{
			name: "test InfiniBand Partition inventory recovers failed partition with error history",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st6.ID,
				infiniBandPartitionInventory: &corev1.InfiniBandPartitionInventory{
					IbPartitions: []*corev1.IBPartition{
						{
							Id: &corev1.IBPartitionId{Value: failedRecoveredIbpID.String()},
							Config: &corev1.IBPartitionConfig{
								Name:                 "failed-site-only-ibp",
								TenantOrganizationId: tn.Org,
							},
							Status: &corev1.IBPartitionStatus{
								State: corev1.TenantState_FAILED,
							},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			check: func(t *testing.T) {
				t.Helper()

				recovered, rerr := cdbm.NewInfiniBandPartitionDAO(dbSession).GetByID(ctx, nil, failedRecoveredIbpID, nil)
				require.NoError(t, rerr)
				require.NotNil(t, recovered)
				assert.Equal(t, cdbm.InfiniBandPartitionStatusError, recovered.Status)

				statusDetails, total, rerr := cdbm.NewStatusDetailDAO(dbSession).GetAll(
					ctx,
					nil,
					cdbm.StatusDetailFilterInput{EntityIDs: []string{recovered.ID.String()}},
					cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
				)
				require.NoError(t, rerr)
				assert.Equal(t, 1, total)
				if assert.Len(t, statusDetails, 1) {
					assert.Equal(t, string(cdbm.InfiniBandPartitionStatusError), statusDetails[0].Status)
					assert.Equal(t, cutil.GetPtr("InfiniBand Partition was found on Site"), statusDetails[0].Message)
				}
			},
		},
		{
			name: "test InfiniBand Partition inventory restores soft-deleted partition",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st4.ID,
				infiniBandPartitionInventory: &corev1.InfiniBandPartitionInventory{
					IbPartitions: []*corev1.IBPartition{
						{
							Id: &corev1.IBPartitionId{Value: restoredControllerID.String()},
							Config: &corev1.IBPartitionConfig{
								Name:                 softDeletedIbp.ID.String(),
								TenantOrganizationId: tn.Org,
							},
							Metadata: &corev1.Metadata{Name: softDeletedIbp.ID.String()},
							Status: &corev1.IBPartitionStatus{
								State:       corev1.TenantState_READY,
								Pkey:        cutil.GetPtr("0x333"),
								EnableSharp: cutil.GetPtr(false),
							},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			check: func(t *testing.T) {
				t.Helper()

				restored, rerr := cdbm.NewInfiniBandPartitionDAO(dbSession).GetByID(ctx, nil, softDeletedIbp.ID, nil)
				require.NoError(t, rerr)

				if !assert.NotNil(t, restored) {
					return
				}

				assert.Nil(t, restored.Deleted)
				assert.Equal(t, softDeletedIbp.Name, restored.Name)
				assert.Nil(t, restored.Description)
				assert.Nil(t, restored.Labels)
				assert.Equal(t, restoredControllerID, *restored.ControllerIBPartitionID)
				assert.Equal(t, "0x333", *restored.PartitionKey)
				assert.Nil(t, restored.PartitionName)
				assert.Nil(t, restored.ServiceLevel)
				assert.Nil(t, restored.RateLimit)
				assert.Nil(t, restored.Mtu)
				require.NotNil(t, restored.EnableSharp)
				assert.False(t, *restored.EnableSharp)
				assert.False(t, restored.IsMissingOnSite)
				assert.Equal(t, cdbm.InfiniBandPartitionStatusReady, restored.Status)

				statusDetails, total, rerr := cdbm.NewStatusDetailDAO(dbSession).GetAll(
					ctx,
					nil,
					cdbm.StatusDetailFilterInput{EntityIDs: []string{restored.ID.String()}},
					cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
				)
				require.NoError(t, rerr)
				assert.Equal(t, 1, total)

				if assert.Len(t, statusDetails, 1) {
					assert.Equal(t, string(cdbm.InfiniBandPartitionStatusReady), statusDetails[0].Status)
					assert.Equal(t, cutil.GetPtr("InfiniBand Partition was found on Site, ready for use"), statusDetails[0].Message)
				}
			},
		},
		{
			name: "test InfiniBand Partition inventory skips terminal Site-only partition",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st5.ID,
				infiniBandPartitionInventory: &corev1.InfiniBandPartitionInventory{
					IbPartitions: []*corev1.IBPartition{
						{
							Id: &corev1.IBPartitionId{Value: terminalIbpID.String()},
							Config: &corev1.IBPartitionConfig{
								Name:                 "terminating-site-only-ibp",
								TenantOrganizationId: tn.Org,
							},
							Status: &corev1.IBPartitionStatus{
								State: corev1.TenantState_TERMINATING,
							},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			check: func(t *testing.T) {
				t.Helper()

				_, rerr := cdbm.NewInfiniBandPartitionDAO(dbSession).GetByID(ctx, nil, terminalIbpID, nil)
				assert.Equal(t, cdb.ErrDoesNotExist, rerr)
			},
		},
		{
			name: "test paged InfiniBand Partition inventory processing, empty inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st1.ID,
				infiniBandPartitionInventory: &corev1.InfiniBandPartitionInventory{
					IbPartitions:    []*corev1.IBPartition{},
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
			name: "test paged InfiniBand Partition inventory processing, first page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				infiniBandPartitionInventory: &corev1.InfiniBandPartitionInventory{
					IbPartitions: pagedCtrlIbps[0:10],
					Timestamp:    timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			readyInfiniBandPartitions: pagedIbps[0:34],
		},
		{
			name: "test paged InfiniBand Partition inventory processing, last page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				infiniBandPartitionInventory: &corev1.InfiniBandPartitionInventory{
					IbPartitions: pagedCtrlIbps[30:34],
					Timestamp:    timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 4,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			readyInfiniBandPartitions:   pagedIbps[0:34],
			missingInfiniBandPartitions: pagedIbps[34:38],
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mibp := ManageInfiniBandPartition{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tSiteClientPool,
			}

			// Mock the "CreateInfiniBandPartition" workflow
			mtc := &tmocks.Client{}
			mibp.siteClientPool.IDClientMap[st1.ID.String()] = mtc

			err := mibp.UpdateInfiniBandPartitionsInDB(tt.args.ctx, tt.args.siteID, tt.args.infiniBandPartitionInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			ibpDAO := cdbm.NewInfiniBandPartitionDAO(dbSession)
			// Check that InfiniBandPartition status was updated in DB for InfiniBandPartition1
			if tt.updatedInfiniBandPartition != nil {
				updatedInfiniBandPartition, _ := ibpDAO.GetByID(ctx, nil, tt.updatedInfiniBandPartition.ID, nil)
				assert.Equal(t, cdbm.InfiniBandPartitionStatusProvisioning, updatedInfiniBandPartition.Status)
				assert.Equal(t, *tt.args.infiniBandPartitionInventory.IbPartitions[0].Status.Pkey, *updatedInfiniBandPartition.PartitionKey)
				assert.Equal(t, *tt.args.infiniBandPartitionInventory.IbPartitions[0].Status.Partition, *updatedInfiniBandPartition.PartitionName)
				assert.Equal(t, int(*tt.args.infiniBandPartitionInventory.IbPartitions[0].Status.ServiceLevel), *updatedInfiniBandPartition.ServiceLevel)
				assert.Equal(t, float32(*tt.args.infiniBandPartitionInventory.IbPartitions[0].Status.RateLimit), *updatedInfiniBandPartition.RateLimit)
				assert.Equal(t, int(*tt.args.infiniBandPartitionInventory.IbPartitions[0].Status.Mtu), *updatedInfiniBandPartition.Mtu)
				assert.Equal(t, *tt.args.infiniBandPartitionInventory.IbPartitions[0].Status.EnableSharp, *updatedInfiniBandPartition.EnableSharp)
			}

			for _, ibp := range tt.readyInfiniBandPartitions {
				uibp, _ := ibpDAO.GetByID(ctx, nil, ibp.ID, nil)

				assert.False(t, uibp.IsMissingOnSite)
				assert.Equal(t, cdbm.InfiniBandPartitionStatusReady, uibp.Status)
			}

			for _, ibp := range tt.deletedInfiniBandPartitions {
				_, err = ibpDAO.GetByID(ctx, nil, ibp.ID, nil)
				require.Equal(t, cdb.ErrDoesNotExist, err, fmt.Sprintf("InfiniBandPartition %s should have been deleted", ibp.Name))
			}

			for _, ibp := range tt.missingInfiniBandPartitions {
				uv, _ := ibpDAO.GetByID(ctx, nil, ibp.ID, nil)

				if uv.ControllerIBPartitionID != nil {
					assert.True(t, uv.IsMissingOnSite)
					assert.Equal(t, cdbm.InfiniBandPartitionStatusError, uv.Status)
				} else {
					assert.False(t, uv.IsMissingOnSite)
				}
			}

			for _, ibp := range tt.unpairedInfiniBandPartitions {
				uv, _ := ibpDAO.GetByID(ctx, nil, ibp.ID, nil)
				assert.NotNil(t, uv.ControllerIBPartitionID)
				if ibp.Status != cdbm.InfiniBandPartitionStatusDeleting {
					assert.Equal(t, cdbm.InfiniBandPartitionStatusReady, uv.Status)
				}
			}

			if tt.restoredInfiniBandPartition != nil {
				rv, _ := ibpDAO.GetByID(ctx, nil, tt.restoredInfiniBandPartition.ID, nil)
				assert.False(t, rv.IsMissingOnSite)
				assert.Equal(t, cdbm.InfiniBandPartitionStatusReady, rv.Status)
			}

			if tt.check != nil {
				tt.check(t)
			}
		})
	}
}

func TestNewManageInfiniBandPartition(t *testing.T) {
	type args struct {
		dbSession      *cdb.Session
		cfg            *config.Config
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
		want ManageInfiniBandPartition
	}{
		{
			name: "test new ManageInfiniBandPartition instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageInfiniBandPartition{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageInfiniBandPartition(tt.args.dbSession, tt.args.siteClientPool); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageInfiniBandPartition() = %v, want %v", got, tt.want)
			}
		})
	}
}
