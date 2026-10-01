// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"context"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"
)

type fixture struct {
	session *cdb.Session
	site    *cdbm.Site
	tenant  *cdbm.Tenant
	manager ManageSitePrefix
}

func setup(t *testing.T) fixture {
	t.Helper()
	session := util.TestInitDB(t)
	t.Cleanup(session.Close)
	util.TestSetupSchema(t, session)
	storage := cipam.NewBunStorage(session.DB, nil)
	require.NoError(t, storage.ApplyDbSchema())
	require.NoError(t, storage.DeleteAllPrefixesFromAllNamespaces(context.Background()))
	user := util.TestBuildUser(t, session, uuid.NewString(), []string{"provider"}, []string{"FORGE_PROVIDER_ADMIN"})
	provider := util.TestBuildInfrastructureProvider(t, session, "provider", "provider", user)
	site := util.TestBuildSite(t, session, provider, "site", cdbm.SiteStatusRegistered, nil, user)
	tenant := util.TestBuildTenant(t, session, "tenant", "tenant", nil, user)
	return fixture{session, site, tenant, NewManageSitePrefix(session)}
}

func (f fixture) inventorySite(t *testing.T) *cdbm.Site {
	t.Helper()
	site, err := cdbm.NewSiteDAO(f.session).GetByID(context.Background(), nil, f.site.ID, nil, false)
	require.NoError(t, err)
	return site
}

func (f fixture) blocks(t *testing.T) []cdbm.IPBlock {
	t.Helper()
	blocks, _, err := cdbm.NewIPBlockDAO(f.session).GetAll(context.Background(), nil,
		cdbm.IPBlockFilterInput{IncludeDeleted: true}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	return blocks
}

func (f fixture) root(t *testing.T, linked bool) *cdbm.IPBlock {
	t.Helper()
	var id *uuid.UUID
	if linked {
		id = cutil.GetPtr(uuid.New())
	}
	block, err := cdbm.NewIPBlockDAO(f.session).Create(context.Background(), nil, cdbm.IPBlockCreateInput{
		Name: "existing-root", Prefix: "10.0.0.0", PrefixLength: 24,
		SiteID: f.site.ID, InfrastructureProviderID: f.site.InfrastructureProviderID,
		RoutingType: cdbm.IPBlockRoutingTypeDatacenterOnly, ProtocolVersion: cdbm.IPBlockProtocolVersionV4,
		SitePrefixID: id, Status: cdbm.IPBlockStatusReady, CreatedBy: &f.site.CreatedBy,
	})
	require.NoError(t, err)
	_, err = ipam.CreateIpamEntryForIPBlock(context.Background(), ipam.NewIpamStorage(f.session.DB, nil),
		"10.0.0.0", 24, block.RoutingType, block.InfrastructureProviderID.String(), block.SiteID.String())
	require.NoError(t, err)
	return block
}

func (f fixture) namespace() string {
	return ipam.GetIpamNamespaceForIPBlock(context.Background(), cdbm.IPBlockRoutingTypeDatacenterOnly,
		f.site.InfrastructureProviderID.String(), f.site.ID.String())
}

func TestManageSitePrefix_UpdateSitePrefixesInDB(t *testing.T) {
	ctx := context.Background()
	tests := []struct {
		name  string
		check func(*testing.T, fixture)
	}{
		{"out of order pages allow increasing estimates and retain complete receipts", func(t *testing.T, f fixture) {
			ids := []string{uuid.NewString(), uuid.NewString(), uuid.NewString(), uuid.NewString()}
			slices.Sort(ids)
			timestamp := timestamppb.Now()
			for _, index := range []int{2, 0, 1} {
				inventory := testInventory(testPrefix())
				inventory.Timestamp, inventory.SitePrefixes = timestamp, nil
				positions := [][]int{{0, 1}, {2}, {3}}[index]
				for _, position := range positions {
					prefix := testPrefix()
					prefix.Id.Value = ids[position]
					prefix.Config.Prefix = []string{"10.0.0.0/24", "10.1.0.0/24", "10.2.0.0/24", "10.3.0.0/24"}[position]
					inventory.SitePrefixes = append(inventory.SitePrefixes, prefix)
				}
				inventory.InventoryPage = &corev1.InventoryPage{CurrentPage: int32(index + 1), TotalPages: 3,
					TotalItems: 4, PageSize: int32(len(positions)), ItemIds: ids}
				if index == 0 {
					inventory.InventoryPage.TotalPages = 2
				}
				require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			}
			progress := f.inventorySite(t).SitePrefixInventoryProgress
			require.Len(t, progress.Pages, 3)
			require.EqualValues(t, 3, progress.FinalPage)
			require.Equal(t, ids, progress.ItemIDs)
			require.Len(t, f.blocks(t), 4)
			require.Nil(t, f.inventorySite(t).SitePrefixInventoryObservedAt)
		}},
		{"cross page inconsistencies preserve accepted receipt", func(t *testing.T, f fixture) {
			ids := []string{uuid.NewString(), uuid.NewString(), uuid.NewString()}
			slices.Sort(ids)
			first := testInventory(testPrefix())
			first.SitePrefixes[0].Id.Value = ids[0]
			first.InventoryPage = &corev1.InventoryPage{CurrentPage: 1, TotalPages: 2, TotalItems: 3, PageSize: 2, ItemIds: ids}
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, first))
			progress := f.inventorySite(t).SitePrefixInventoryProgress
			for _, change := range []struct {
				name      string
				apply     func(*corev1.SitePrefixInventory)
				errorText string
			}{
				{"duplicate", func(i *corev1.SitePrefixInventory) { i.SitePrefixes[0].Id.Value = ids[0] }, "more than one page"},
				{"missing item", func(_ *corev1.SitePrefixInventory) {}, "does not match declared IDs"},
				{"changed IDs", func(i *corev1.SitePrefixInventory) {
					i.InventoryPage.ItemIds = i.InventoryPage.ItemIds[:2]
					i.InventoryPage.TotalItems = 2
				}, "declared IDs"},
			} {
				t.Run(change.name, func(t *testing.T) {
					next := proto.Clone(first).(*corev1.SitePrefixInventory)
					next.InventoryPage.CurrentPage = 2
					next.SitePrefixes[0].Id.Value = ids[1]
					next.SitePrefixes[0].Config.Prefix = "10.1.0.0/24"
					change.apply(next)
					require.ErrorContains(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, next), change.errorText)
					require.Equal(t, progress, f.inventorySite(t).SitePrefixInventoryProgress)
					require.Len(t, f.blocks(t), 1)
				})
			}
		}},
		{"Site deletion commits before reconciliation", func(t *testing.T, f fixture) { checkSiteDeletion(t, f, false) }},
		{"reconciliation commits before Site deletion", func(t *testing.T, f fixture) { checkSiteDeletion(t, f, true) }},
		{"allocation creation can finish while inventory waits for its root", checkAllocationCreation},
		{"operator creates root and IPAM", func(t *testing.T, f fixture) {
			inventory := testInventory(testPrefix())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, inventory.SitePrefixes[0].Id.Value, blocks[0].SitePrefixID.String())
			require.Equal(t, inventory.SitePrefixes[0].Metadata.Name, blocks[0].Name)
			require.Equal(t, &inventory.SitePrefixes[0].Metadata.Description, blocks[0].Description)
			_, err := ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, "10.0.0.0/24", f.namespace())
			require.NoError(t, err)
			site := f.inventorySite(t)
			require.Nil(t, site.SitePrefixInventoryObservedAt)
			require.Len(t, site.SitePrefixInventoryProgress.Pages, 1)
		}},
		{"unnamed operator root uses CIDR and absent description", func(t *testing.T, f fixture) {
			prefix := testPrefix()
			prefix.Metadata = &corev1.Metadata{}
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, prefix.Config.Prefix, blocks[0].Name)
			require.Nil(t, blocks[0].Description)
		}},
		{"adopts root preserving REST identity and allocated child", func(t *testing.T, f fixture) {
			root := f.root(t, false)
			allocator := cipam.NewWithStorage(ipam.NewIpamStorage(f.session.DB, nil))
			allocator.SetNamespace(f.namespace())
			child, err := allocator.AcquireChildPrefix(ctx, "10.0.0.0/24", 28)
			require.NoError(t, err)
			prefix := testPrefix()
			prefix.Metadata = &corev1.Metadata{}
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, root.ID, blocks[0].ID)
			require.NotNil(t, blocks[0].SitePrefixID)
			require.Equal(t, root.Name, blocks[0].Name)
			require.Nil(t, blocks[0].Description)
			retained, err := ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, child.Cidr, f.namespace())
			require.NoError(t, err)
			require.Equal(t, *child, retained)
		}},
		{"linked operator preserves absent metadata and applies populated metadata", func(t *testing.T, f fixture) {
			root := f.root(t, true)
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			inventory := testInventory(prefix)
			for _, step := range []struct {
				name     string
				metadata *corev1.Metadata
			}{
				{"populated", prefix.Metadata},
				{"absent", &corev1.Metadata{}},
			} {
				t.Run(step.name, func(t *testing.T) {
					prefix.Metadata = step.metadata
					inventory.Timestamp = timestamppb.New(inventory.Timestamp.AsTime().Add(time.Second))
					require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
					blocks := f.blocks(t)
					require.Len(t, blocks, 1)
					require.Equal(t, root.ID, blocks[0].ID)
					require.Equal(t, "test-prefix", blocks[0].Name)
					require.Equal(t, cutil.GetPtr("reported by Core"), blocks[0].Description)
				})
			}
		}},
		{"tenant metadata updates can clear the description", func(t *testing.T, f fixture) {
			prefix := testPrefix()
			prefix.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
			prefix.Config.TenantOrganizationId = &f.tenant.Org
			inventory := testInventory(prefix)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			block := f.blocks(t)[0]
			require.Equal(t, &prefix.Metadata.Description, block.Description)
			prefix.Metadata.Name = "renamed tenant prefix"
			prefix.Metadata.Description = ""
			inventory.Timestamp = timestamppb.New(inventory.Timestamp.AsTime().Add(time.Second))
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, block.ID, blocks[0].ID)
			require.Equal(t, prefix.Metadata.Name, blocks[0].Name)
			require.Equal(t, cutil.GetPtr(""), blocks[0].Description)
		}},
		{"tenant lifecycle and overlapping tenant ownership use no cloud IPAM", func(t *testing.T, f fixture) {
			prefix := testPrefix()
			prefix.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
			prefix.Config.TenantOrganizationId = &f.tenant.Org
			inventory := testInventory(prefix)
			for index, status := range []string{cdbm.IPBlockStatusProvisioning, cdbm.IPBlockStatusReady, cdbm.IPBlockStatusDeleting, cdbm.IPBlockStatusError} {
				prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState(index + 1)
				inventory.Timestamp = timestamppb.New(time.Now().Add(time.Duration(index) * time.Millisecond))
				require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
				blocks := f.blocks(t)
				require.Len(t, blocks, 1)
				require.Equal(t, &f.tenant.ID, blocks[0].TenantID)
				require.Equal(t, status, blocks[0].Status)
			}
			other := util.TestBuildTenant(t, f.session, "other", "other", nil, &cdbm.User{ID: f.site.CreatedBy})
			prefix.Id.Value = uuid.NewString()
			prefix.Config.TenantOrganizationId = &other.Org
			inventory = testInventory(prefix)
			inventory.Timestamp = timestamppb.New(time.Now().Add(time.Second))
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Len(t, f.blocks(t), 2)
			prefixes, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixes(ctx, f.namespace())
			require.NoError(t, err)
			require.Empty(t, prefixes)
		}},
		{"operator replacement is received but deferred", func(t *testing.T, f fixture) {
			root := f.root(t, true)
			inventory := testInventory(testPrefix())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, root.SitePrefixID, blocks[0].SitePrefixID)
			require.Equal(t, root.Name, blocks[0].Name)
			receipt := f.inventorySite(t).SitePrefixInventoryProgress.Pages[1]
			require.Equal(t, inventory.InventoryPage.ItemIds, receipt.ItemIDs)
			require.Equal(t, receipt.ItemIDs, receipt.DeferredIDs)
		}},
		{"exact retry noops and changed retry rejects", func(t *testing.T, f fixture) {
			inventory := testInventory(testPrefix())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			site, block := f.inventorySite(t), f.blocks(t)[0]
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, proto.Clone(inventory).(*corev1.SitePrefixInventory)))
			require.Equal(t, site.Updated, f.inventorySite(t).Updated)
			require.Equal(t, block.Updated, f.blocks(t)[0].Updated)
			inventory.SitePrefixes[0].Metadata.Name = "changed"
			require.ErrorContains(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory), "retry changed")
			require.Equal(t, block.Name, f.blocks(t)[0].Name)
		}},
		{"unchanged later collection records receipt without rewriting the block", func(t *testing.T, f fixture) {
			inventory := testInventory(testPrefix())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			block := f.blocks(t)[0]
			inventory.Timestamp = timestamppb.New(inventory.Timestamp.AsTime().Add(time.Second))
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Equal(t, block, f.blocks(t)[0])
			progress := f.inventorySite(t).SitePrefixInventoryProgress
			require.Equal(t, inventory.Timestamp.AsTime(), progress.ReportedAt)
			hash, err := validatePage(inventory)
			require.NoError(t, err)
			require.Equal(t, hash, progress.Pages[1].Hash)
			details, _, err := cdbm.NewStatusDetailDAO(f.session).GetAll(ctx, nil,
				cdbm.StatusDetailFilterInput{EntityIDs: []string{block.ID.String()}}, paginator.PageInput{})
			require.NoError(t, err)
			require.Len(t, details, 1)
		}},
		{"older collection cannot overwrite or infer absence", func(t *testing.T, f fixture) {
			inventory := testInventory(testPrefix())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			progress := f.inventorySite(t).SitePrefixInventoryProgress
			inventory.Timestamp = timestamppb.New(inventory.Timestamp.AsTime().Add(-time.Second))
			inventory.SitePrefixes[0].Metadata.Name = "stale"
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Equal(t, progress, f.inventorySite(t).SitePrefixInventoryProgress)
			inventory.Timestamp = timestamppb.New(time.Now().Add(time.Second))
			inventory.SitePrefixes, inventory.InventoryPage.ItemIds = nil, nil
			inventory.InventoryPage.TotalItems, inventory.InventoryPage.TotalPages = 0, 0
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Len(t, f.blocks(t), 1)
			require.Nil(t, f.blocks(t)[0].Deleted)
			require.Nil(t, f.inventorySite(t).SitePrefixInventoryObservedAt)
		}},
		{"late final page in the current collection is reconciled", func(t *testing.T, f fixture) {
			ids := []string{uuid.NewString(), uuid.NewString()}
			slices.Sort(ids)
			first := testInventory(testPrefix())
			first.SitePrefixes[0].Id.Value = ids[0]
			first.InventoryPage = &corev1.InventoryPage{CurrentPage: 1, TotalPages: 2, TotalItems: 2, PageSize: 1, ItemIds: ids}
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, first))

			// Backdate the accepted collection and its receipt to model four
			// minutes passing without sleeping.
			first.Timestamp = timestamppb.New(time.Now().Add(-4 * time.Minute))
			progress := f.inventorySite(t).SitePrefixInventoryProgress
			progress.ReportedAt = first.Timestamp.AsTime()
			receipt := progress.Pages[1]
			hash, err := validatePage(first)
			require.NoError(t, err)
			receipt.Hash = hash
			progress.Pages[1] = receipt
			_, err = cdbm.NewSiteDAO(f.session).Update(ctx, nil, cdbm.SiteUpdateInput{
				SiteID: f.site.ID, SitePrefixInventoryProgress: progress,
			})
			require.NoError(t, err)

			final := proto.Clone(first).(*corev1.SitePrefixInventory)
			final.InventoryPage.CurrentPage = 2
			final.SitePrefixes[0].Id.Value = ids[1]
			final.SitePrefixes[0].Config.Prefix = "10.1.0.0/24"
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, final))
			block, err := cdbm.NewIPBlockDAO(f.session).GetBySitePrefixID(ctx, nil, uuid.MustParse(ids[1]))
			require.NoError(t, err)
			require.Equal(t, "10.1.0.0", block.Prefix)
			saved := f.inventorySite(t).SitePrefixInventoryProgress
			require.Equal(t, progress.ReportedAt, saved.ReportedAt)
			require.Len(t, saved.Pages, 2)
			require.Equal(t, receipt, saved.Pages[1])
			hash, err = validatePage(final)
			require.NoError(t, err)
			require.Equal(t, cdbm.SitePrefixInventoryPage{
				Hash: hash, ItemIDs: []string{ids[1]}, DeferredIDs: []string{},
			}, saved.Pages[2])
			require.EqualValues(t, 2, saved.FinalPage)
			require.Nil(t, f.inventorySite(t).SitePrefixInventoryObservedAt)
		}},
		{"failure diagnostics do not change progress or resources", func(t *testing.T, f fixture) {
			inventory := testInventory(testPrefix())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			site := f.inventorySite(t)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, &corev1.SitePrefixInventory{
				Timestamp: timestamppb.Now(), InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED, StatusMsg: "Core unavailable",
			}))
			require.Equal(t, site, f.inventorySite(t))
			require.Len(t, f.blocks(t), 1)
		}},
		{"later unknown tenant rolls back earlier resource and IPAM", func(t *testing.T, f fixture) {
			first, second := testPrefix(), testPrefix()
			ids := []string{first.Id.Value, second.Id.Value}
			slices.Sort(ids)
			first.Id.Value, second.Id.Value = ids[0], ids[1]
			second.Config.TenantOrganizationId = cutil.GetPtr("unknown")
			second.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
			inventory := testInventory(first)
			inventory.SitePrefixes = append(inventory.SitePrefixes, second)
			inventory.InventoryPage.ItemIds, inventory.InventoryPage.TotalItems = ids, 2
			require.ErrorContains(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory), "unknown or ambiguous")
			require.Empty(t, f.blocks(t))
			require.Nil(t, f.inventorySite(t).SitePrefixInventoryProgress)
			prefixes, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixes(ctx, f.namespace())
			require.NoError(t, err)
			require.Empty(t, prefixes)
		}},
		{"soft deleted Core ID cannot be reused", func(t *testing.T, f fixture) {
			root := f.root(t, true)
			require.NoError(t, cdbm.NewIPBlockDAO(f.session).Delete(ctx, nil, root.ID))
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			require.ErrorContains(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)), "immutable REST identity")
			require.Nil(t, f.inventorySite(t).SitePrefixInventoryProgress)
			require.NotNil(t, f.blocks(t)[0].Deleted)
		}},
		{"Core identity cannot move between owners", func(t *testing.T, f fixture) {
			root := f.root(t, true)
			user := &cdbm.User{ID: f.site.CreatedBy}
			provider := util.TestBuildInfrastructureProvider(t, f.session, "other-provider", "other-provider", user)
			site := util.TestBuildSite(t, f.session, provider, "other-site", cdbm.SiteStatusRegistered, nil, user)
			tenant := util.TestBuildTenant(t, f.session, "other-tenant", "other-tenant", nil, user)
			for _, change := range []struct {
				name  string
				apply func(*cdbm.IPBlock, *corev1.SitePrefix)
			}{
				{"Site", func(b *cdbm.IPBlock, _ *corev1.SitePrefix) { b.SiteID = site.ID }},
				{"provider", func(b *cdbm.IPBlock, _ *corev1.SitePrefix) { b.InfrastructureProviderID = provider.ID }},
				{"tenant", func(b *cdbm.IPBlock, p *corev1.SitePrefix) {
					b.TenantID = &f.tenant.ID
					p.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
					p.Config.TenantOrganizationId = &tenant.Org
				}},
			} {
				t.Run(change.name, func(t *testing.T) {
					changed := *root
					prefix := testPrefix()
					prefix.Id.Value = root.SitePrefixID.String()
					change.apply(&changed, prefix)
					_, err := f.session.DB.NewUpdate().Model(&changed).
						Column("site_id", "infrastructure_provider_id", "tenant_id").WherePK().Exec(ctx)
					require.NoError(t, err)
					require.ErrorContains(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)), "immutable REST identity")
					require.Nil(t, f.inventorySite(t).SitePrefixInventoryProgress)
					require.Equal(t, root.Name, f.blocks(t)[0].Name)
				})
			}
		}},
		{"contention returns and a later attempt succeeds", func(t *testing.T, f fixture) {
			tx, err := cdb.BeginTx(ctx, f.session, nil)
			require.NoError(t, err)
			defer func() { _ = tx.Rollback() }()
			require.NoError(t, tx.AcquireAdvisoryLock(ctx, cdbm.SiteFabricIPBlockLockID(f.site.InfrastructureProviderID, f.site.ID), false))
			inventory := testInventory(testPrefix())
			require.ErrorIs(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory), cdb.ErrXactAdvisoryLockFailed)
			require.Empty(t, f.blocks(t))
			require.Nil(t, f.inventorySite(t).SitePrefixInventoryProgress)
			require.NoError(t, tx.Commit())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Len(t, f.blocks(t), 1)
		}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) { tt.check(t, setup(t)) })
	}
	for _, replacementFirst := range []bool{false, true} {
		name := "resize lifecycle before replacement"
		if replacementFirst {
			name = "resize replacement before lifecycle"
		}
		t.Run(name, func(t *testing.T) {
			f := setup(t)
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
			replacement.Config.Prefix = "10.0.0.0/25"
			inventory := testInventory(old)
			inventory.SitePrefixes = []*corev1.SitePrefix{old, replacement}
			if replacementFirst {
				inventory.SitePrefixes = []*corev1.SitePrefix{replacement, old}
			}
			inventory.InventoryPage.ItemIds = ids
			inventory.InventoryPage.TotalItems = 2
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, root.ID, blocks[0].ID)
			require.Equal(t, cdbm.IPBlockStatusDeleting, blocks[0].Status)
			receipt := f.inventorySite(t).SitePrefixInventoryProgress.Pages[1]
			require.Equal(t, ids, receipt.ItemIDs)
			require.Equal(t, []string{replacement.Id.Value}, receipt.DeferredIDs)
			tx, err := cdb.BeginTx(ctx, f.session, nil)
			require.NoError(t, err)
			defer func() { _ = tx.Rollback() }()
			require.ErrorContains(t, ipam.LockAndValidateParentIPBlockForAllocation(ctx, tx, f.session, root), "not Ready")
		})
	}

	for _, change := range []struct {
		name      string
		apply     func(*cdbm.IPBlock)
		errorText string
	}{
		{"routing mismatch", func(b *cdbm.IPBlock) { b.RoutingType = cdbm.IPBlockRoutingTypePublic }, "incompatible adoption"},
		{"family mismatch", func(b *cdbm.IPBlock) { b.ProtocolVersion = cdbm.IPBlockProtocolVersionV6 }, "incompatible adoption"},
		{"overlapping root", func(b *cdbm.IPBlock) { b.PrefixLength = 16 }, "overlaps"},
		{"ambiguous roots", func(_ *cdbm.IPBlock) {}, "ambiguous adoption"},
	} {
		t.Run(change.name, func(t *testing.T) {
			f := setup(t)
			root := f.root(t, false)
			change.apply(root)
			if change.name == "ambiguous roots" {
				root.ID = uuid.New()
				_, err := f.session.DB.NewInsert().Model(root).Exec(ctx)
				require.NoError(t, err)
			} else {
				_, err := f.session.DB.NewUpdate().Model(root).Column("routing_type", "protocol_version", "prefix_length").WherePK().Exec(ctx)
				require.NoError(t, err)
			}
			require.ErrorContains(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(testPrefix())), change.errorText)
			require.Nil(t, f.inventorySite(t).SitePrefixInventoryProgress)
			for _, block := range f.blocks(t) {
				require.Nil(t, block.SitePrefixID)
			}
		})
	}

	for _, change := range []struct {
		name  string
		apply func(*corev1.SitePrefix)
	}{
		{"CIDR", func(p *corev1.SitePrefix) { p.Config.Prefix = "10.1.0.0/24" }},
		{"family", func(p *corev1.SitePrefix) { p.Config.Prefix = "fd00::/64" }},
		{"authority", func(p *corev1.SitePrefix) {
			p.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
			p.Config.TenantOrganizationId = cutil.GetPtr("tenant")
		}},
	} {
		t.Run("reject immutable "+change.name, func(t *testing.T) {
			f := setup(t)
			root := f.root(t, true)
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			change.apply(prefix)
			require.ErrorContains(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)), "immutable REST identity")
			require.Nil(t, f.inventorySite(t).SitePrefixInventoryProgress)
			require.Equal(t, root.Name, f.blocks(t)[0].Name)
		})
	}
}

func checkAllocationCreation(t *testing.T, f fixture) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	root := f.root(t, true)
	tx, err := cdb.BeginTx(ctx, f.session, nil)
	require.NoError(t, err)
	defer func() { _ = tx.Rollback() }()
	require.NoError(t, ipam.LockAndValidateParentIPBlockForAllocation(ctx, tx, f.session, root))
	var allocatorPID int
	require.NoError(t, tx.GetBunTx().NewSelect().ColumnExpr("pg_backend_pid()").Scan(ctx, &allocatorPID))
	prefix := testPrefix()
	prefix.Id.Value = root.SitePrefixID.String()
	prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
	done := make(chan error, 1)
	go func() { done <- f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)) }()
	completed := false
	defer func() {
		cancel()
		_ = tx.Rollback()
		if !completed {
			<-done
		}
	}()
	require.Eventually(t, func() bool {
		var waiters int
		err := f.session.DB.NewSelect().ColumnExpr("count(*)").TableExpr("pg_catalog.pg_stat_activity").
			Where("? = ANY(pg_blocking_pids(pid))", allocatorPID).
			Where("query LIKE ?", "%\"ip_block\"%FOR UPDATE%").Scan(ctx, &waiters)
		return err == nil && waiters > 0
	}, 5*time.Second, 10*time.Millisecond, "inventory must hold Site while waiting for allocation's root")
	_, insertErr := cdbm.NewIPBlockDAO(f.session).Create(ctx, tx, cdbm.IPBlockCreateInput{
		Name: "allocated-child", Prefix: root.Prefix, PrefixLength: 25,
		SiteID: f.site.ID, InfrastructureProviderID: f.site.InfrastructureProviderID, TenantID: &f.tenant.ID,
		RoutingType: root.RoutingType, ProtocolVersion: root.ProtocolVersion,
		Status: cdbm.IPBlockStatusReady, CreatedBy: &f.site.CreatedBy,
	})
	var commitErr error
	if insertErr == nil {
		commitErr = tx.Commit()
	} else {
		_ = tx.Rollback()
	}
	inventoryErr := <-done
	completed = true
	require.NoError(t, insertErr)
	require.NoError(t, commitErr)
	require.NoError(t, inventoryErr)
	require.Len(t, f.blocks(t), 2)
	updated, err := cdbm.NewIPBlockDAO(f.session).GetByID(ctx, nil, root.ID, nil)
	require.NoError(t, err)
	require.Equal(t, cdbm.IPBlockStatusDeleting, updated.Status)
	require.Len(t, f.inventorySite(t).SitePrefixInventoryProgress.Pages, 1)
}

// Pause after acquiring the Site row lock, without timing-dependent sleeps.
type siteLockHook struct {
	entered chan struct{}
	resume  chan struct{}
	once    sync.Once
}

func (hook *siteLockHook) BeforeQuery(ctx context.Context, _ *bun.QueryEvent) context.Context {
	return ctx
}

func (hook *siteLockHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
	if event.Err == nil && strings.Contains(event.Query, "FROM \"site\" AS \"st\"") && strings.Contains(event.Query, "FOR NO KEY UPDATE") {
		hook.once.Do(func() {
			close(hook.entered)
			select {
			case <-hook.resume:
			case <-ctx.Done():
			}
		})
	}
}

func checkSiteDeletion(t *testing.T, f fixture, reconcileFirst bool) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	dao := cdbm.NewSiteDAO(f.session)
	inventory := testInventory(testPrefix())
	done := make(chan error, 1)
	if reconcileFirst {
		hook := &siteLockHook{entered: make(chan struct{}), resume: make(chan struct{})}
		f.session.DB.AddQueryHook(hook)
		var release sync.Once
		defer release.Do(func() { close(hook.resume) })
		go func() { done <- f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory) }()
		select {
		case <-hook.entered:
		case <-ctx.Done():
			t.Fatal("receiver did not acquire Site lock")
		}
		deleted := make(chan error, 1)
		go func() { deleted <- dao.Delete(ctx, nil, f.site.ID) }()
		require.Eventually(t, func() bool {
			var waiters int
			err := f.session.DB.NewSelect().ColumnExpr("count(*)").TableExpr("pg_catalog.pg_stat_activity").
				Where("datname = current_database() AND cardinality(pg_blocking_pids(pid)) > 0").
				Where("query LIKE ?", "%\"site\"%").Scan(ctx, &waiters)
			return err == nil && waiters > 0
		}, 5*time.Second, 10*time.Millisecond, "Site deletion must wait for inventory")
		release.Do(func() { close(hook.resume) })
		require.NoError(t, <-done)
		require.NoError(t, <-deleted)
		require.Len(t, f.blocks(t), 1)
	} else {
		tx, err := cdb.BeginTx(ctx, f.session, nil)
		require.NoError(t, err)
		defer func() { _ = tx.Rollback() }()
		var writerPID int
		require.NoError(t, tx.GetBunTx().NewSelect().ColumnExpr("pg_backend_pid()").Scan(ctx, &writerPID))
		require.NoError(t, dao.Delete(ctx, tx, f.site.ID))
		go func() { done <- f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory) }()
		require.Eventually(t, func() bool {
			var waiters int
			err := f.session.DB.NewSelect().ColumnExpr("count(*)").TableExpr("pg_catalog.pg_stat_activity").
				Where("? = ANY(pg_blocking_pids(pid))", writerPID).Scan(ctx, &waiters)
			return err == nil && waiters > 0
		}, 5*time.Second, 10*time.Millisecond, "inventory must reload Site under its row lock")
		require.NoError(t, tx.Commit())
		require.ErrorIs(t, <-done, cdb.ErrDoesNotExist)
		require.Empty(t, f.blocks(t))
	}
	require.ErrorIs(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory), cdb.ErrDoesNotExist)
	deletedSite, err := dao.GetByID(ctx, nil, f.site.ID, nil, true)
	require.NoError(t, err)
	require.NotNil(t, deletedSite.Deleted)
	if !reconcileFirst {
		require.Nil(t, deletedSite.SitePrefixInventoryProgress)
	}
}
