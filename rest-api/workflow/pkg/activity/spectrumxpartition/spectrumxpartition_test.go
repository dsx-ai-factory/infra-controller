// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package spectrumxpartition

import (
	"context"
	"fmt"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// spectrumXReconcilerFixture is the Provider, Tenant and Site chain a Partition needs.
type spectrumXReconcilerFixture struct {
	tenant *cdbm.Tenant
	site   *cdbm.Site
}

func testBuildReconcilerFixture(t *testing.T, dbSession *cdb.Session, siteName string) spectrumXReconcilerFixture {
	ipOrg := "test-provider-org-" + siteName
	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{"FORGE_PROVIDER_ADMIN"})
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-provider-"+siteName, ipOrg, ipu)

	tnOrg := "test-tenant-org-" + siteName
	tnu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg}, []string{"FORGE_TENANT_ADMIN"})
	tn := util.TestBuildTenant(t, dbSession, "Test Tenant "+siteName, tnOrg, nil, tnu)
	require.NotNil(t, tn)

	st := util.TestBuildSite(t, dbSession, ip, siteName, cdbm.SiteStatusRegistered, nil, ipu)
	require.NotNil(t, st)

	return spectrumXReconcilerFixture{tenant: tn, site: st}
}

// ageRow backdates `created` and `updated` past the stale inventory threshold, which is the
// gate the deletion sweep and missing-on-Site flagging both sit behind.
func ageRow(t *testing.T, dbSession *cdb.Session, partitionID uuid.UUID) {
	past := time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval) * 2)
	_, err := dbSession.DB.Exec("UPDATE spectrumx_partition SET created = ?, updated = ? WHERE id = ?", past, past, partitionID.String())
	require.NoError(t, err)
}

func reportedPartition(id uuid.UUID, vni uint32) *corev1.SpxPartition {
	return &corev1.SpxPartition{
		Id:  &corev1.SpxPartitionId{Value: id.String()},
		Vni: vni,
	}
}

// TestManageSpectrumXPartition_UpdateSpectrumXPartitionsInDB covers the branches the
// reconciler owns. Because forge.SpxPartition has no status sub-message, presence in the
// inventory is the only signal the Site gives, so each case asserts the persisted row after
// the iteration rather than relying on an absent external action.
func TestManageSpectrumXPartition_UpdateSpectrumXPartitionsInDB(t *testing.T) {
	ctx := context.Background()
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	fx := testBuildReconcilerFixture(t, dbSession, "test-site-1")
	sxpDAO := cdbm.NewSpectrumXPartitionDAO(dbSession)
	manager := NewManageSpectrumXPartition(dbSession, nil)

	t.Run("unknown Site returns an error", func(t *testing.T) {
		err := manager.UpdateSpectrumXPartitionsInDB(ctx, uuid.New(), &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
		})
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})

	// A failed collection must not be read as "the Site reports nothing", or every
	// Partition would be swept as missing on a transient Core outage.
	t.Run("failed inventory status leaves rows untouched", func(t *testing.T) {
		sxp := util.TestBuildSpectrumXPartition(t, dbSession, "failed-inv", fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusReady, false)
		ageRow(t, dbSession, sxp.ID)

		err := manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
			Timestamp:       timestamppb.Now(),
		})
		require.NoError(t, err)

		persisted, err := sxpDAO.Get(ctx, nil, sxp.ID, nil)
		require.NoError(t, err)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusReady, persisted.Status)
		assert.False(t, persisted.IsMissingOnSite)

		require.NoError(t, sxpDAO.Delete(ctx, nil, sxp.ID))
	})

	t.Run("reported Pending Partition is promoted to Ready and records its controller ID and VNI", func(t *testing.T) {
		sxp := util.TestBuildSpectrumXPartition(t, dbSession, "pending-to-ready", fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusPending, false)

		err := manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
			SpxPartitions:   []*corev1.SpxPartition{reportedPartition(sxp.ID, 10200)},
			InventoryPage:   &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 1, TotalItems: 1, ItemIds: []string{sxp.ID.String()}},
		})
		require.NoError(t, err)

		persisted, err := sxpDAO.Get(ctx, nil, sxp.ID, nil)
		require.NoError(t, err)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusReady, persisted.Status)
		require.NotNil(t, persisted.VNI, "Site-allocated VNI must be recorded")
		assert.Equal(t, 10200, *persisted.VNI)

		require.NoError(t, sxpDAO.Delete(ctx, nil, sxp.ID))
	})

	// Being reported again is the only way back from Error, so the missing flag has to clear.
	t.Run("reported Partition that was flagged missing is restored", func(t *testing.T) {
		sxp := util.TestBuildSpectrumXPartition(t, dbSession, "restored", fx.site, fx.tenant, cutil.GetPtr(10300), cdbm.SpectrumXPartitionStatusError, true)

		err := manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
			SpxPartitions:   []*corev1.SpxPartition{reportedPartition(sxp.ID, 10300)},
			InventoryPage:   &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 1, TotalItems: 1, ItemIds: []string{sxp.ID.String()}},
		})
		require.NoError(t, err)

		persisted, err := sxpDAO.Get(ctx, nil, sxp.ID, nil)
		require.NoError(t, err)
		assert.False(t, persisted.IsMissingOnSite)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusReady, persisted.Status)

		require.NoError(t, sxpDAO.Delete(ctx, nil, sxp.ID))
	})

	// A Deleting Partition the Site still reports is mid-teardown, so its status must
	// survive the iteration rather than being promoted back to Ready.
	t.Run("reported Deleting Partition keeps its Deleting status", func(t *testing.T) {
		sxp := util.TestBuildSpectrumXPartition(t, dbSession, "still-deleting", fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusDeleting, false)

		err := manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
			SpxPartitions:   []*corev1.SpxPartition{reportedPartition(sxp.ID, 0)},
			InventoryPage:   &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 1, TotalItems: 1, ItemIds: []string{sxp.ID.String()}},
		})
		require.NoError(t, err)

		persisted, err := sxpDAO.Get(ctx, nil, sxp.ID, nil)
		require.NoError(t, err)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusDeleting, persisted.Status)

		require.NoError(t, sxpDAO.Delete(ctx, nil, sxp.ID))
	})

	t.Run("Deleting Partition absent from inventory is removed", func(t *testing.T) {
		sxp := util.TestBuildSpectrumXPartition(t, dbSession, "gone-deleting", fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusDeleting, false)

		err := manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
			InventoryPage:   &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 0, TotalItems: 0},
		})
		require.NoError(t, err)

		_, err = sxpDAO.Get(ctx, nil, sxp.ID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})

	t.Run("Ready Partition absent from inventory is flagged missing and moved to Error", func(t *testing.T) {
		sxp := util.TestBuildSpectrumXPartition(t, dbSession, "gone-ready", fx.site, fx.tenant, cutil.GetPtr(10400), cdbm.SpectrumXPartitionStatusReady, false)
		ageRow(t, dbSession, sxp.ID)

		err := manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
			InventoryPage:   &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 0, TotalItems: 0},
		})
		require.NoError(t, err)

		persisted, err := sxpDAO.Get(ctx, nil, sxp.ID, nil)
		require.NoError(t, err)
		assert.True(t, persisted.IsMissingOnSite)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusError, persisted.Status)

		require.NoError(t, sxpDAO.Delete(ctx, nil, sxp.ID))
	})

	// A Partition created moments ago has not had time to reach the Site, so absence
	// from this cycle's inventory is expected rather than a fault.
	t.Run("freshly created Partition absent from inventory is left alone", func(t *testing.T) {
		sxp := util.TestBuildSpectrumXPartition(t, dbSession, "fresh", fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusPending, false)

		err := manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
			InventoryPage:   &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 0, TotalItems: 0},
		})
		require.NoError(t, err)

		persisted, err := sxpDAO.Get(ctx, nil, sxp.ID, nil)
		require.NoError(t, err)
		assert.False(t, persisted.IsMissingOnSite)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusPending, persisted.Status)

		require.NoError(t, sxpDAO.Delete(ctx, nil, sxp.ID))
	})

	// The cases below cover Partitions the Site reports that REST has no active row for. The
	// Site is the source of truth, so such a Partition is created under the Site-reported ID,
	// or undeleted if a soft-deleted row already carries that ID. They share one Partition and
	// run in order, so a failure early on stops the chain rather than cascading.
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)

	siteCreatedID := uuid.New()
	siteCreated := &corev1.SpxPartition{
		Id:                   &corev1.SpxPartitionId{Value: siteCreatedID.String()},
		Vni:                  10500,
		TenantOrganizationId: fx.tenant.Org,
		Metadata: &corev1.Metadata{
			Name:        "site-created-partition",
			Description: "created directly on Site",
			Labels:      []*corev1.Label{{Key: "origin", Value: cutil.GetPtr("site")}},
		},
	}
	inventory := &corev1.SpectrumXPartitionInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Timestamp:       timestamppb.Now(),
		SpxPartitions:   []*corev1.SpxPartition{siteCreated},
		InventoryPage:   &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 1, TotalItems: 1, ItemIds: []string{siteCreatedID.String()}},
	}

	if !t.Run("auto creates Partition from inventory", func(t *testing.T) {
		require.NoError(t, manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, inventory))

		created, err := sxpDAO.Get(ctx, nil, siteCreatedID, nil)
		require.NoError(t, err)
		assert.Equal(t, siteCreatedID, created.ID)
		assert.Equal(t, "site-created-partition", created.Name)
		require.NotNil(t, created.Description)
		assert.Equal(t, "created directly on Site", *created.Description)
		assert.Equal(t, fx.tenant.Org, created.Org)
		assert.Equal(t, fx.tenant.ID, created.TenantID)
		assert.Equal(t, fx.site.ID, created.SiteID)
		assert.Equal(t, fx.tenant.CreatedBy, created.CreatedBy)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusReady, created.Status)
		assert.False(t, created.IsMissingOnSite)
		assert.Equal(t, cdbm.Labels{"origin": "site"}, created.Labels)
		require.NotNil(t, created.VNI)
		assert.Equal(t, 10500, *created.VNI)

		details, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{siteCreatedID.String()}, 10)
		require.NoError(t, err)
		require.Len(t, details, 1)
		require.NotNil(t, details[0].Message)
		assert.Equal(t, "SpectrumX Partition was found on Site, Ready for use", *details[0].Message)
	}) {
		t.FailNow()
	}

	if !t.Run("inventory replay is idempotent", func(t *testing.T) {
		require.NoError(t, manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, inventory))

		rows, count, err := sxpDAO.GetAll(ctx, nil, cdbm.SpectrumXPartitionFilterInput{SpectrumXPartitionIDs: []uuid.UUID{siteCreatedID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		require.NoError(t, err)
		require.Len(t, rows, 1)
		assert.Equal(t, 1, count)

		details, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{siteCreatedID.String()}, 10)
		require.NoError(t, err)
		assert.Len(t, details, 1, "a replay must not append another status detail")
	}) {
		t.FailNow()
	}

	// A row only reaches soft-deletion from Deleting, so that is the state a Site-reported
	// Partition is undeleted from. The undelete must bring it back to Ready itself, because
	// the main loop deliberately leaves Deleting rows alone.
	t.Run("inventory restores soft-deleted Partition", func(t *testing.T) {
		deletingStatus := cdbm.SpectrumXPartitionStatusDeleting
		_, err := sxpDAO.Update(ctx, nil, cdbm.SpectrumXPartitionUpdateInput{
			SpectrumXPartitionID: siteCreatedID,
			Status:               &deletingStatus,
			IsMissingOnSite:      cutil.GetPtr(true),
		})
		require.NoError(t, err)
		require.NoError(t, sxpDAO.Delete(ctx, nil, siteCreatedID))

		deleted, _, err := sxpDAO.GetAll(ctx, nil, cdbm.SpectrumXPartitionFilterInput{SpectrumXPartitionIDs: []uuid.UUID{siteCreatedID}, IncludeDeleted: true}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		require.NoError(t, err)
		require.Len(t, deleted, 1)
		require.NotNil(t, deleted[0].Deleted)

		// A delete newer than the staleness threshold may postdate this inventory, so the
		// undelete is deferred until the delete has aged past it.
		require.NoError(t, manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, inventory))
		_, err = sxpDAO.Get(ctx, nil, siteCreatedID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist, "a recent delete must not be undone yet")

		util.TestInventoryAgeDeletedTimestamp(ctx, t, dbSession, (*cdbm.SpectrumXPartition)(nil), siteCreatedID)
		require.NoError(t, manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, inventory))

		restored, err := sxpDAO.Get(ctx, nil, siteCreatedID, nil)
		require.NoError(t, err)
		assert.Nil(t, restored.Deleted)
		assert.False(t, restored.IsMissingOnSite)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusReady, restored.Status)
		require.NotNil(t, restored.VNI)
		assert.Equal(t, 10500, *restored.VNI)

		// The most recent status detail is the undelete's, so the history shows why the
		// Partition left Deleting.
		details, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{siteCreatedID.String()}, 1)
		require.NoError(t, err)
		require.Len(t, details, 1)
		assert.Equal(t, string(cdbm.SpectrumXPartitionStatusReady), details[0].Status)
		require.NotNil(t, details[0].Message)
		assert.Equal(t, "SpectrumX Partition was found on Site, Ready for use", *details[0].Message)
	})

	t.Run("inventory skips restore when tenant organization differs", func(t *testing.T) {
		require.NoError(t, sxpDAO.Delete(ctx, nil, siteCreatedID))
		util.TestInventoryAgeDeletedTimestamp(ctx, t, dbSession, (*cdbm.SpectrumXPartition)(nil), siteCreatedID)

		mismatched := &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
			SpxPartitions: []*corev1.SpxPartition{{
				Id:                   &corev1.SpxPartitionId{Value: siteCreatedID.String()},
				TenantOrganizationId: "other-tenant-org",
				Metadata:             &corev1.Metadata{Name: "site-created-partition"},
			}},
			InventoryPage: &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 1, TotalItems: 1, ItemIds: []string{siteCreatedID.String()}},
		}
		require.NoError(t, manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, mismatched))

		_, err := sxpDAO.Get(ctx, nil, siteCreatedID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})

	t.Run("name conflict gets a recovered suffix", func(t *testing.T) {
		existing := util.TestBuildSpectrumXPartition(t, dbSession, "shared-name", fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusReady, false)
		newID := uuid.New()

		require.NoError(t, manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
			SpxPartitions: []*corev1.SpxPartition{
				reportedPartition(existing.ID, 0),
				{
					Id:                   &corev1.SpxPartitionId{Value: newID.String()},
					TenantOrganizationId: fx.tenant.Org,
					Metadata:             &corev1.Metadata{Name: "shared-name"},
				},
			},
			InventoryPage: &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 2, TotalItems: 2, ItemIds: []string{existing.ID.String(), newID.String()}},
		}))

		created, err := sxpDAO.Get(ctx, nil, newID, nil)
		require.NoError(t, err)
		assert.Equal(t, fmt.Sprintf("shared-name-recovered-%s", newID.String()[:8]), created.Name)

		require.NoError(t, sxpDAO.Delete(ctx, nil, existing.ID))
		require.NoError(t, sxpDAO.Delete(ctx, nil, newID))
	})

	t.Run("missing name falls back to a recovered name", func(t *testing.T) {
		newID := uuid.New()

		require.NoError(t, manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
			SpxPartitions: []*corev1.SpxPartition{{
				Id:                   &corev1.SpxPartitionId{Value: newID.String()},
				TenantOrganizationId: fx.tenant.Org,
			}},
			InventoryPage: &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 1, TotalItems: 1, ItemIds: []string{newID.String()}},
		}))

		created, err := sxpDAO.Get(ctx, nil, newID, nil)
		require.NoError(t, err)
		assert.Equal(t, fmt.Sprintf("recovered-%s", newID.String()[:8]), created.Name)
		assert.Nil(t, created.Description)
		assert.Nil(t, created.VNI)

		require.NoError(t, sxpDAO.Delete(ctx, nil, newID))
	})

	// Partitions whose ownership cannot be established are skipped rather than guessed at.
	t.Run("skips Partitions without a resolvable Tenant or a valid ID", func(t *testing.T) {
		unknownTenantID := uuid.New()
		emptyOrgID := uuid.New()

		require.NoError(t, manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
			SpxPartitions: []*corev1.SpxPartition{
				{Id: &corev1.SpxPartitionId{Value: unknownTenantID.String()}, TenantOrganizationId: "unknown-tenant-org", Metadata: &corev1.Metadata{Name: "unknown-tenant"}},
				{Id: &corev1.SpxPartitionId{Value: emptyOrgID.String()}, Metadata: &corev1.Metadata{Name: "empty-org"}},
				{Id: &corev1.SpxPartitionId{Value: "not-a-uuid"}, TenantOrganizationId: fx.tenant.Org, Metadata: &corev1.Metadata{Name: "bad-id"}},
			},
			InventoryPage: &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 3, TotalItems: 3},
		}))

		_, err := sxpDAO.Get(ctx, nil, unknownTenantID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
		_, err = sxpDAO.Get(ctx, nil, emptyOrgID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)

		rows, _, err := sxpDAO.GetAll(ctx, nil, cdbm.SpectrumXPartitionFilterInput{SiteIDs: []uuid.UUID{fx.site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		require.NoError(t, err)
		assert.Empty(t, rows)
	})
}

// TestManageSpectrumXPartition_DeletionSweepOnlyOnLastPage proves the sweep is deferred
// until the final page. A Partition that Cloud holds but the Site never reports is the only
// row the sweep acts on, so the test keeps one such orphan and asserts it survives the
// non-final page and is acted on exactly once when the final page arrives.
func TestManageSpectrumXPartition_DeletionSweepOnlyOnLastPage(t *testing.T) {
	ctx := context.Background()
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	fx := testBuildReconcilerFixture(t, dbSession, "test-site-paged")
	sxpDAO := cdbm.NewSpectrumXPartitionDAO(dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)
	manager := NewManageSpectrumXPartition(dbSession, nil)

	// The Site reports four Partitions across two pages.
	reported := []*cdbm.SpectrumXPartition{}
	for i := range 4 {
		sxp := util.TestBuildSpectrumXPartition(t, dbSession, fmt.Sprintf("paged-%d", i), fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusReady, false)
		ageRow(t, dbSession, sxp.ID)
		reported = append(reported, sxp)
	}

	// Cloud also holds a Partition the Site does not report at all. This is what the
	// sweep exists to catch, and what running it early would touch on every page.
	orphan := util.TestBuildSpectrumXPartition(t, dbSession, "orphan", fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusReady, false)
	ageRow(t, dbSession, orphan.ID)

	// A Deleting Partition the Site does not report should be removed by the sweep, and
	// only once the final page confirms it really is gone.
	deleting := util.TestBuildSpectrumXPartition(t, dbSession, "orphan-deleting", fx.site, fx.tenant, nil, cdbm.SpectrumXPartitionStatusDeleting, false)
	ageRow(t, dbSession, deleting.ID)

	reportedIDs := []string{}
	for _, sxp := range reported {
		reportedIDs = append(reportedIDs, sxp.ID.String())
	}

	buildPage := func(currentPage int32, items []*cdbm.SpectrumXPartition) *corev1.SpectrumXPartitionInventory {
		protos := []*corev1.SpxPartition{}
		for _, sxp := range items {
			protos = append(protos, reportedPartition(sxp.ID, 0))
		}
		return &corev1.SpectrumXPartitionInventory{
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			Timestamp:       timestamppb.Now(),
			SpxPartitions:   protos,
			InventoryPage:   &corev1.InventoryPage{TotalPages: 2, CurrentPage: currentPage, PageSize: 2, TotalItems: 4, ItemIds: reportedIDs},
		}
	}

	require.NoError(t, manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, buildPage(1, reported[:2])))

	// Page one is not the last, so neither orphan is acted on yet.
	persistedOrphan, err := sxpDAO.Get(ctx, nil, orphan.ID, nil)
	require.NoError(t, err)
	assert.False(t, persistedOrphan.IsMissingOnSite, "the sweep must not run on a non-final page")
	assert.Equal(t, cdbm.SpectrumXPartitionStatusReady, persistedOrphan.Status)

	_, err = sxpDAO.Get(ctx, nil, deleting.ID, nil)
	require.NoError(t, err, "a Deleting Partition must survive a non-final page")

	require.NoError(t, manager.UpdateSpectrumXPartitionsInDB(ctx, fx.site.ID, buildPage(2, reported[2:])))

	// The final page confirms both orphans are gone from the Site.
	persistedOrphan, err = sxpDAO.Get(ctx, nil, orphan.ID, nil)
	require.NoError(t, err)
	assert.True(t, persistedOrphan.IsMissingOnSite)
	assert.Equal(t, cdbm.SpectrumXPartitionStatusError, persistedOrphan.Status)

	// Exactly one Error status detail, so a repeated sweep cannot pile up history.
	details, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{orphan.ID.String()}, 10)
	require.NoError(t, err)
	assert.Len(t, details, 1)

	_, err = sxpDAO.Get(ctx, nil, deleting.ID, nil)
	assert.ErrorIs(t, err, cdb.ErrDoesNotExist)

	// Everything the Site did report stays untouched.
	for _, sxp := range reported {
		persisted, err := sxpDAO.Get(ctx, nil, sxp.ID, nil)
		require.NoError(t, err)
		assert.False(t, persisted.IsMissingOnSite, "%s is in the item list, so it must not be flagged", sxp.Name)
	}
}
