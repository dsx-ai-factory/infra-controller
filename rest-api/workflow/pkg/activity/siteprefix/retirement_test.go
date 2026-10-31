// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"context"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"slices"
	"strings"
	"testing"
	"time"
)

func checkSitePrefixRetirement(t *testing.T) {
	ctx := context.Background()
	cases := []struct {
		name  string
		check func(*testing.T, testFixture)
	}{
		{"reported Deleting removes free root and restores same identity", func(t *testing.T, f testFixture) {
			root := f.root(t, true)
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			require.NotNil(t, f.blocks(t)[0].Deleted)
			_, err := ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, "10.0.0.0/24", f.namespace())
			require.Error(t, err)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			require.NotNil(t, f.blocks(t)[0].Deleted)
			prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			require.Nil(t, f.blocks(t)[0].Deleted)
			require.Equal(t, root.ID, f.blocks(t)[0].ID)
			_, err = ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, "10.0.0.0/24", f.namespace())
			require.NoError(t, err)
		}},
		{"Deleting adopts and retires a compatible unlinked root regardless of its name", func(t *testing.T, f testFixture) {
			root := f.root(t, false)
			prefix := testPrefix()
			prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			retired := f.blocks(t)[0]
			require.Equal(t, root.ID, retired.ID)
			require.Equal(t, root.Name, retired.Name)
			require.Equal(t, prefix.Id.Value, retired.SitePrefixID.String())
			require.Equal(t, cdbm.IPBlockStatusDeleting, retired.Status)
			require.NotNil(t, retired.Deleted)
			_, err := ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, "10.0.0.0/24", f.namespace())
			require.Error(t, err)
		}},
		{"savepoint release failure aborts lifecycle and cleanup then retries", func(t *testing.T, f testFixture) {
			root := f.root(t, true)
			hook := &testRetirementSavepointFailureHook{armed: true}
			f.session.DB.AddQueryHook(hook)
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			inventory := testInventory(prefix)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.False(t, hook.armed, "the savepoint release must have been attempted")
			require.Equal(t, *root, f.blocks(t)[0])
			details, _, err := cdbm.NewStatusDetailDAO(f.session).GetAll(ctx, nil,
				cdbm.StatusDetailFilterInput{EntityIDs: []string{root.ID.String()}}, paginator.PageInput{})
			require.NoError(t, err)
			require.Empty(t, details)
			_, err = ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, "10.0.0.0/24", f.namespace())
			require.NoError(t, err)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.NotNil(t, f.blocks(t)[0].Deleted)
			_, err = ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, "10.0.0.0/24", f.namespace())
			require.Error(t, err)
		}},
		{"last page alone removes old missing root", func(t *testing.T, f testFixture) {
			root := f.root(t, true)
			_, err := f.session.DB.NewUpdate().Model((*cdbm.IPBlock)(nil)).Set("created = ?", time.Now().Add(-time.Hour)).Where("id = ?", root.ID).Exec(ctx)
			require.NoError(t, err)
			inventory := testInventory(testPrefix())
			inventory.SitePrefixes = nil
			inventory.InventoryPage = &corev1.InventoryPage{CurrentPage: 2, TotalPages: 2, TotalItems: 1, ItemIds: []string{uuid.NewString()}}
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.NotNil(t, f.blocks(t)[0].Deleted)
		}},
		{"invalid payload preserves its reported root without blocking absence cleanup", func(t *testing.T, f testFixture) {
			reportedID, missingID := uuid.New(), uuid.New()
			f.createExistingIPBlocks(t, []testExistingIPBlock{
				{cidr: "10.0.0.0/24", name: "reported-root", sitePrefixID: reportedID.String()},
				{cidr: "10.1.0.0/24", name: "missing-root", sitePrefixID: missingID.String()},
			})
			for _, block := range f.blocks(t) {
				ageBlock(t, f, &block)
			}
			before := f.blocksByID(t)
			prefix := testPrefix()
			prefix.Id.Value = reportedID.String()
			prefix.Config = nil
			inventory := testInventory(prefix)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			reported, err := f.blockByPrefixID(ctx, reportedID)
			require.NoError(t, err)
			require.Equal(t, before[reported.ID], *reported)
			missing, err := f.blockByPrefixID(ctx, missingID)
			require.NoError(t, err)
			require.Equal(t, cdbm.IPBlockStatusDeleting, missing.Status)
			require.NotNil(t, missing.Deleted)
			require.Equal(t, []string{"10.0.0.0/24"}, f.ipamCIDRs(t, cdbm.IPBlockRoutingTypeDatacenterOnly))
		}},
		{"collection recovers within its schedule", checkCollectionSchedule},
		{"missing allocated root clears its link and is adopted by a new Core ID", func(t *testing.T, f testFixture) {
			root := f.root(t, true)
			ageBlock(t, f, root)
			allocator := cipam.NewWithStorage(ipam.NewIpamStorage(f.session.DB, nil))
			allocator.SetNamespace(f.namespace())
			child, err := allocator.AcquireChildPrefix(ctx, "10.0.0.0/24", 28)
			require.NoError(t, err)
			inventory := &corev1.SitePrefixInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			retained := f.blocks(t)[0]
			require.Nil(t, retained.Deleted)
			require.Nil(t, retained.SitePrefixID)
			require.Equal(t, cdbm.IPBlockStatusDeleting, retained.Status)
			prefix := testPrefix()
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			adopted := f.blocks(t)[0]
			require.Equal(t, root.ID, adopted.ID)
			require.Equal(t, prefix.Id.Value, adopted.SitePrefixID.String())
			require.Equal(t, cdbm.IPBlockStatusReady, adopted.Status)
			preserved, err := ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, child.Cidr, f.namespace())
			require.NoError(t, err)
			require.Equal(t, *child, preserved)
		}},
		{"absence only sweeps eligible linked blocks", func(t *testing.T, f testFixture) {
			for _, row := range []struct {
				name                            string
				managed, tenant, linked, recent bool
				status, wantStatus              string
				wantDeleted                     bool
			}{
				{"unlinked operator", true, false, false, false, cdbm.IPBlockStatusReady, cdbm.IPBlockStatusReady, false},
				{"allocated tenant", true, true, true, false, cdbm.IPBlockStatusReady, cdbm.IPBlockStatusReady, false},
				{"missing tenant", false, true, true, false, cdbm.IPBlockStatusReady, cdbm.IPBlockStatusError, false},
				{"retiring tenant", false, true, true, false, cdbm.IPBlockStatusDeleting, cdbm.IPBlockStatusDeleting, true},
				{"recent retiring tenant", false, true, true, true, cdbm.IPBlockStatusDeleting, cdbm.IPBlockStatusDeleting, false},
			} {
				t.Run(row.name, func(t *testing.T) {
					var tenantID, prefixID *uuid.UUID
					if row.tenant {
						tenantID = &f.tenants["tenant"].ID
					}
					if row.linked {
						prefixID = cutil.GetPtr(uuid.New())
					}
					dao := cdbm.NewIPBlockDAO(f.session)
					block, err := dao.Create(ctx, nil, cdbm.IPBlockCreateInput{
						Name: row.name, Prefix: "10.1.0.0", PrefixLength: 24, SiteID: f.site.ID,
						InfrastructureProviderID: f.site.InfrastructureProviderID, TenantID: tenantID,
						SitePrefixID: prefixID, Managed: &row.managed, Status: row.status,
						RoutingType: cdbm.IPBlockRoutingTypeDatacenterOnly, ProtocolVersion: cdbm.IPBlockProtocolVersionV4,
					})
					require.NoError(t, err)
					if !row.recent {
						ageBlock(t, f, block)
					}
					inventory := &corev1.SitePrefixInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
						InventoryPage: &corev1.InventoryPage{CurrentPage: 1}}
					require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
					all := f.blocks(t)
					index := slices.IndexFunc(all, func(b cdbm.IPBlock) bool { return b.ID == block.ID })
					require.NotEqual(t, -1, index)
					require.Equal(t, row.wantStatus, all[index].Status)
					require.Equal(t, row.wantDeleted, all[index].Deleted != nil)
					require.Equal(t, prefixID, all[index].SitePrefixID)
				})
			}
		}},
		{"nonfinal and failed inventories preserve old missing root", func(t *testing.T, f testFixture) {
			root := f.root(t, true)
			ageBlock(t, f, root)
			before := f.blocks(t)
			for _, inventory := range []*corev1.SitePrefixInventory{
				{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, InventoryPage: &corev1.InventoryPage{CurrentPage: 1, TotalPages: 2, TotalItems: 1}},
				{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, InventoryPage: &corev1.InventoryPage{CurrentPage: 2, TotalPages: 2, TotalItems: 1}},
				{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED},
			} {
				require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
				require.Equal(t, before, f.blocks(t))
			}
		}},
		{"final page IDs protect earlier payloads", func(t *testing.T, f testFixture) {
			root := f.root(t, true)
			ageBlock(t, f, root)
			before := f.blocks(t)
			inventory := &corev1.SitePrefixInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				InventoryPage: &corev1.InventoryPage{CurrentPage: 2, TotalPages: 2, TotalItems: 1, ItemIds: []string{root.SitePrefixID.String()}}}
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Equal(t, before, f.blocks(t))
		}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) { tc.check(t, newTestFixture(t)) })
	}
	for _, missing := range []bool{false, true} {
		name := "reported Deleting preserves lifecycle when cleanup fails then retries"
		if missing {
			name = "absence preserves lifecycle when cleanup fails then retries"
		}
		t.Run(name, func(t *testing.T) {
			f := newTestFixture(t)
			root := f.root(t, true)
			ageBlock(t, f, root)
			_, err := f.session.DB.ExecContext(ctx, `CREATE FUNCTION reject_prefix_retirement() RETURNS trigger AS $$ BEGIN IF NEW.deleted IS NOT NULL THEN RAISE EXCEPTION 'injected retirement failure'; END IF; RETURN NEW; END $$ LANGUAGE plpgsql`)
			require.NoError(t, err)
			_, err = f.session.DB.ExecContext(ctx, `CREATE TRIGGER reject_prefix_retirement BEFORE UPDATE ON ip_block FOR EACH ROW EXECUTE FUNCTION reject_prefix_retirement()`)
			require.NoError(t, err)
			t.Cleanup(func() {
				_, cleanupErr := f.session.DB.ExecContext(ctx, `DROP FUNCTION reject_prefix_retirement() CASCADE`)
				require.NoError(t, cleanupErr)
			})
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			inventory := testInventory(prefix)
			if missing {
				inventory = &corev1.SitePrefixInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}
			}
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			retained := f.blocks(t)[0]
			require.Nil(t, retained.Deleted)
			require.Equal(t, root.SitePrefixID, retained.SitePrefixID)
			require.Equal(t, cdbm.IPBlockStatusDeleting, retained.Status)
			details, _, err := cdbm.NewStatusDetailDAO(f.session).GetAll(ctx, nil,
				cdbm.StatusDetailFilterInput{EntityIDs: []string{root.ID.String()}}, paginator.PageInput{})
			require.NoError(t, err)
			require.Len(t, details, 1)
			require.Equal(t, cdbm.IPBlockStatusDeleting, details[0].Status)
			message := sitePrefixInventoryStatusMessage
			if missing {
				message = sitePrefixMissingMessage
			}
			require.Equal(t, &message, details[0].Message)
			_, err = ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, "10.0.0.0/24", f.namespace())
			require.NoError(t, err)
			_, err = f.session.DB.ExecContext(ctx, `DROP TRIGGER reject_prefix_retirement ON ip_block`)
			require.NoError(t, err)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.NotNil(t, f.blocks(t)[0].Deleted)
			_, err = ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, "10.0.0.0/24", f.namespace())
			require.Error(t, err)
		})
	}
	for _, replacementFirst := range []bool{false, true} {
		name := "resize lifecycle before replacement"
		if replacementFirst {
			name = "resize replacement before lifecycle"
		}
		t.Run(name, func(t *testing.T) {
			f := newTestFixture(t)
			root := f.root(t, true)
			ids := []string{root.SitePrefixID.String(), uuid.NewString()}
			slices.Sort(ids)
			oldIndex := 0
			if replacementFirst {
				oldIndex = 1
			}
			root.SitePrefixID = cutil.GetPtr(uuid.MustParse(ids[oldIndex]))
			_, err := f.session.DB.NewUpdate().Model(root).Column("site_prefix_id").WherePK().Exec(ctx)
			require.NoError(t, err)
			old := testPrefix()
			old.Id.Value = ids[oldIndex]
			old.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			replacement := testPrefix()
			replacement.Id.Value = ids[1-oldIndex]
			replacement.Config.Prefix = "10.0.0.0/23"
			inventory := testInventory(old)
			inventory.SitePrefixes = []*corev1.SitePrefix{old, replacement}
			if replacementFirst {
				inventory.SitePrefixes = []*corev1.SitePrefix{replacement, old}
			}
			inventory.InventoryPage.ItemIds = ids
			inventory.InventoryPage.TotalItems = 2
			allocator := cipam.NewWithStorage(ipam.NewIpamStorage(f.session.DB, nil))
			allocator.SetNamespace(f.namespace())
			child, err := allocator.AcquireChildPrefix(ctx, "10.0.0.0/24", 28)
			require.NoError(t, err)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, root.ID, blocks[0].ID)
			require.Equal(t, cdbm.IPBlockStatusDeleting, blocks[0].Status)
			tx, err := cdb.BeginTx(ctx, f.session, nil)
			require.NoError(t, err)
			defer func() { _ = tx.Rollback() }()
			require.ErrorContains(t, ipam.LockAndValidateParentIPBlockForAllocation(ctx, tx, f.session, root), "not Ready")
			require.NoError(t, tx.Rollback())
			err = allocator.ReleaseChildPrefix(ctx, child)
			require.NoError(t, err)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			retired, err := f.blockByPrefixID(ctx, *root.SitePrefixID)
			require.NoError(t, err)
			require.NotNil(t, retired.Deleted)
			created, err := f.blockByPrefixID(ctx, uuid.MustParse(replacement.Id.Value))
			require.NoError(t, err)
			require.Nil(t, created.Deleted)
			require.Equal(t, 23, created.PrefixLength)
			require.Equal(t, cdbm.IPBlockStatusReady, created.Status)
		})
	}

}

func (f testFixture) root(t *testing.T, linked bool) *cdbm.IPBlock {
	t.Helper()
	id := ""
	if linked {
		id = uuid.NewString()
	}
	f.createExistingIPBlocks(t, []testExistingIPBlock{{cidr: "10.0.0.0/24", name: "existing-root", sitePrefixID: id}})
	return &f.blocks(t)[0]
}
func (f testFixture) namespace() string {
	return ipam.GetIpamNamespaceForIPBlock(context.Background(), cdbm.IPBlockRoutingTypeDatacenterOnly, f.site.InfrastructureProviderID.String(), f.site.ID.String())
}
func (f testFixture) blockByPrefixID(ctx context.Context, id uuid.UUID) (*cdbm.IPBlock, error) {
	blocks, _, err := cdbm.NewIPBlockDAO(f.session).GetAll(ctx, nil, cdbm.IPBlockFilterInput{SitePrefixIDs: []uuid.UUID{id}, IncludeDeleted: true}, paginator.PageInput{}, nil)
	if err != nil {
		return nil, err
	}
	if len(blocks) != 1 {
		return nil, cdb.ErrDoesNotExist
	}
	return &blocks[0], nil
}
func testPrefix() *corev1.SitePrefix {
	return (testSitePrefix{id: uuid.NewString(), cidr: "10.0.0.0/24", name: "prefix"}).toProto()
}
func testInventory(prefix *corev1.SitePrefix) *corev1.SitePrefixInventory {
	inventory := testSitePrefixInventory(nil)
	inventory.SitePrefixes = []*corev1.SitePrefix{prefix}
	inventory.InventoryPage.TotalItems = 1
	inventory.InventoryPage.ItemIds = []string{prefix.GetId().GetValue()}
	return inventory
}
func ageBlock(t *testing.T, f testFixture, block *cdbm.IPBlock) {
	t.Helper()
	_, err := f.session.DB.NewUpdate().Model((*cdbm.IPBlock)(nil)).Set("created = ?", time.Now().Add(-time.Hour)).Where("id = ?", block.ID).Exec(context.Background())
	require.NoError(t, err)
}

// Fail one savepoint statement without cancelling the outer transaction context.
type testRetirementSavepointFailureHook struct {
	armed bool
}

func (h *testRetirementSavepointFailureHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if !h.armed || !strings.HasPrefix(event.Query, "RELEASE SAVEPOINT") {
		return ctx
	}
	h.armed = false
	failedCtx, cancel := context.WithCancel(ctx)
	cancel()
	return failedCtx
}

func (*testRetirementSavepointFailureHook) AfterQuery(context.Context, *bun.QueryEvent) {}
