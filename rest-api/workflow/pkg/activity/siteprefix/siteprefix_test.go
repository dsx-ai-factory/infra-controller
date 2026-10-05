// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"bytes"
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
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"go.temporal.io/sdk/temporal"
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

func (f fixture) retiredRoot(t *testing.T) *cdbm.IPBlock {
	t.Helper()
	ctx := context.Background()
	root := f.root(t, true)
	dao := cdbm.NewIPBlockDAO(f.session)
	err := cdb.WithTx(ctx, f.session, func(tx *cdb.Tx) error {
		_, err := dao.Update(ctx, tx, cdbm.IPBlockUpdateInput{IPBlockID: root.ID, Status: cutil.GetPtr(cdbm.IPBlockStatusDeleting)})
		if err != nil {
			return err
		}
		err = dao.Delete(ctx, tx, root.ID)
		if err != nil {
			return err
		}
		return ipam.DeleteIpamEntryForIPBlock(ctx, ipam.NewIpamStorage(f.session.DB, tx.GetBunTx()),
			root.Prefix, root.PrefixLength, root.RoutingType, root.InfrastructureProviderID.String(), root.SiteID.String())
	})
	require.NoError(t, err)
	retired, err := dao.GetBySitePrefixID(ctx, nil, *root.SitePrefixID)
	require.NoError(t, err)
	require.NotNil(t, retired.Deleted)
	return retired
}

func TestManageSitePrefix_UpdateSitePrefixesInDB(t *testing.T) {
	ctx := context.Background()
	tests := []struct {
		name  string
		check func(*testing.T, fixture)
	}{
		{"nil inventory is rejected", func(t *testing.T, f fixture) {
			require.ErrorContains(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, nil), "nil inventory")
		}},
		{"unsupported inventory status is rejected", func(t *testing.T, f fixture) {
			inventory := testInventory(testPrefix())
			inventory.InventoryStatus = corev1.InventoryStatus_INVENTORY_STATUS_UNSPECIFIED
			require.ErrorContains(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory), "unsupported SitePrefix inventory status")
			require.Empty(t, f.blocks(t))
		}},
		{"page metadata does not gate reconciliation", func(t *testing.T, f fixture) {
			inventory := testInventory(testPrefix())
			inventory.Timestamp, inventory.InventoryPage = nil, nil
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			block := f.blocks(t)[0]
			inventory.InventoryPage = &corev1.InventoryPage{TotalItems: -1, ItemIds: []string{"not a prefix ID"}}
			inventory.SitePrefixes[0].Metadata.Name = "updated"
			inventory.SitePrefixes[0].Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, block.ID, blocks[0].ID)
			require.Equal(t, block.Name, blocks[0].Name)
			require.Equal(t, cdbm.IPBlockStatusDeleting, blocks[0].Status)
		}},
		{"out of order pages reconcile independently with increasing estimates", func(t *testing.T, f fixture) {
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
			require.Len(t, f.blocks(t), 4)
		}},
		{"Site deletion commits before reconciliation", func(t *testing.T, f fixture) { checkSiteDeletion(t, f, false) }},
		{"reconciliation commits before Site deletion", func(t *testing.T, f fixture) { checkSiteDeletion(t, f, true) }},
		{"allocation creation can finish while inventory waits for its root", checkAllocationCreation},
		{"Site deletion between prefixes prevents later creation", func(t *testing.T, f fixture) {
			first, second := testPrefix(), testPrefix()
			ids := []string{first.Id.Value, second.Id.Value}
			slices.Sort(ids)
			first.Id.Value, second.Id.Value = ids[0], ids[1]
			second.Config.Prefix = "10.1.0.0/24"
			inventory := testInventory(first)
			inventory.SitePrefixes = append(inventory.SitePrefixes, second)
			inventory.InventoryPage.ItemIds, inventory.InventoryPage.TotalItems = ids, 2
			hook := &siteDeletionHook{session: f.session, siteID: f.site.ID}
			f.session.DB.AddQueryHook(hook)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.NoError(t, hook.err)
			require.Equal(t, 2, hook.reads)
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, first.Id.Value, blocks[0].SitePrefixID.String())
			prefixes, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, f.namespace())
			require.NoError(t, err)
			require.Equal(t, []string{first.Config.Prefix}, prefixes)
		}},
		{"operator creates root and IPAM", func(t *testing.T, f fixture) {
			inventory := testInventory(testPrefix())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, inventory.SitePrefixes[0].Id.Value, blocks[0].SitePrefixID.String())
			require.True(t, blocks[0].Managed)
			require.Equal(t, inventory.SitePrefixes[0].Metadata.Name, blocks[0].Name)
			require.Equal(t, &inventory.SitePrefixes[0].Metadata.Description, blocks[0].Description)
			_, err := ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, "10.0.0.0/24", f.namespace())
			require.NoError(t, err)
		}},
		{"unreconcilable entries do not hold back later prefixes", func(t *testing.T, f fixture) {
			var output bytes.Buffer
			previousLogger := log.Logger
			log.Logger = zerolog.New(&output)
			t.Cleanup(func() { log.Logger = previousLogger })
			malformed, unknown, valid := testPrefix(), testPrefix(), testPrefix()
			malformed.Config.Prefix = "10.1.0.1/24"
			unknown.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
			unknown.Config.TenantOrganizationId = cutil.GetPtr("unknown")
			inventory := testInventory(malformed)
			inventory.SitePrefixes = []*corev1.SitePrefix{nil, malformed, unknown, valid}
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, valid.Id.Value, blocks[0].SitePrefixID.String())
			require.Contains(t, output.String(), malformed.Id.Value)
			require.Contains(t, output.String(), "network-aligned CIDR")
			require.Contains(t, output.String(), unknown.Id.Value)
			require.Contains(t, output.String(), "unknown or ambiguous")
		}},
		{"public operator uses public IPAM namespace", func(t *testing.T, f fixture) {
			prefix := testPrefix()
			prefix.Config.Prefix = "203.0.113.0/24"
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, cdbm.IPBlockRoutingTypePublic, blocks[0].RoutingType)
			namespace := ipam.GetIpamNamespaceForIPBlock(ctx, cdbm.IPBlockRoutingTypePublic,
				f.site.InfrastructureProviderID.String(), f.site.ID.String())
			_, err := ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, prefix.Config.Prefix, namespace)
			require.NoError(t, err)
		}},
		{"public operator adoption and restoration preserve routing and metadata", func(t *testing.T, f fixture) {
			dao := cdbm.NewIPBlockDAO(f.session)
			root, err := dao.Create(ctx, nil, cdbm.IPBlockCreateInput{
				Name: "site-fabric-ipv6-root", Description: cutil.GetPtr("provider description"),
				SiteID: f.site.ID, InfrastructureProviderID: f.site.InfrastructureProviderID,
				Prefix: "2001:db8::", PrefixLength: 48, RoutingType: cdbm.IPBlockRoutingTypePublic,
				ProtocolVersion: cdbm.IPBlockProtocolVersionV6, Status: cdbm.IPBlockStatusReady,
			})
			require.NoError(t, err)
			storage := ipam.NewIpamStorage(f.session.DB, nil)
			_, err = ipam.CreateIpamEntryForIPBlock(ctx, storage, root.Prefix, root.PrefixLength,
				root.RoutingType, root.InfrastructureProviderID.String(), root.SiteID.String())
			require.NoError(t, err)
			prefix := testPrefix()
			prefix.Config.Prefix = "2001:db8::/48"
			prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			adopted, err := dao.GetByID(ctx, nil, root.ID, nil)
			require.NoError(t, err)
			require.Equal(t, prefix.Id.Value, adopted.SitePrefixID.String())
			require.Equal(t, cdbm.IPBlockStatusDeleting, adopted.Status)
			require.Equal(t, root.Name, adopted.Name)
			require.Equal(t, root.Description, adopted.Description)
			// Model retirement without implementing the follow-up cleanup workflow.
			require.NoError(t, dao.Delete(ctx, nil, root.ID))
			require.NoError(t, ipam.DeleteIpamEntryForIPBlock(ctx, storage, root.Prefix, root.PrefixLength,
				root.RoutingType, root.InfrastructureProviderID.String(), root.SiteID.String()))
			prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			restored, err := dao.GetByID(ctx, nil, root.ID, nil)
			require.NoError(t, err)
			require.Equal(t, cdbm.IPBlockStatusReady, restored.Status)
			require.Equal(t, root.RoutingType, restored.RoutingType)
			require.Equal(t, root.Name, restored.Name)
			require.Equal(t, root.Description, restored.Description)
			namespace := ipam.GetIpamNamespaceForIPBlock(ctx, root.RoutingType,
				root.InfrastructureProviderID.String(), root.SiteID.String())
			_, err = storage.ReadPrefix(ctx, prefix.Config.Prefix, namespace)
			require.NoError(t, err)
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
		{"adopts root and applies lifecycle preserving REST identity and allocated child", func(t *testing.T, f fixture) {
			root := f.root(t, false)
			allocator := cipam.NewWithStorage(ipam.NewIpamStorage(f.session.DB, nil))
			allocator.SetNamespace(f.namespace())
			child, err := allocator.AcquireChildPrefix(ctx, "10.0.0.0/24", 28)
			require.NoError(t, err)
			prefix := testPrefix()
			prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, root.ID, blocks[0].ID)
			require.NotNil(t, blocks[0].SitePrefixID)
			require.Equal(t, root.Name, blocks[0].Name)
			require.Nil(t, blocks[0].Description)
			require.Equal(t, cdbm.IPBlockStatusDeleting, blocks[0].Status)
			details, _, err := cdbm.NewStatusDetailDAO(f.session).GetAll(ctx, nil,
				cdbm.StatusDetailFilterInput{EntityIDs: []string{root.ID.String()}}, paginator.PageInput{})
			require.NoError(t, err)
			require.Len(t, details, 1)
			require.Equal(t, cdbm.IPBlockStatusDeleting, details[0].Status)
			retained, err := ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, child.Cidr, f.namespace())
			require.NoError(t, err)
			require.Equal(t, *child, retained)
		}},
		{"linked operator preserves REST metadata", func(t *testing.T, f fixture) {
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
					require.Equal(t, root.Name, blocks[0].Name)
					require.Equal(t, root.Description, blocks[0].Description)
					require.Equal(t, root.Updated, blocks[0].Updated)
				})
			}
		}},
		{"tenant inventory preserves REST metadata while updating lifecycle", func(t *testing.T, f fixture) {
			prefix := testPrefix()
			prefix.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
			prefix.Config.TenantOrganizationId = &f.tenant.Org
			inventory := testInventory(prefix)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			block := f.blocks(t)[0]
			require.Equal(t, &prefix.Metadata.Description, block.Description)
			prefix.Metadata.Name = "renamed tenant prefix"
			prefix.Metadata.Description = ""
			prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			inventory.Timestamp = timestamppb.New(inventory.Timestamp.AsTime().Add(time.Second))
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, block.ID, blocks[0].ID)
			require.Equal(t, block.Name, blocks[0].Name)
			require.Equal(t, block.Description, blocks[0].Description)
			require.Equal(t, cdbm.IPBlockStatusDeleting, blocks[0].Status)
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
				require.False(t, blocks[0].Managed)
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
		{"conflicting operator identity is skipped", func(t *testing.T, f fixture) {
			root := f.root(t, true)
			inventory := testInventory(testPrefix())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, root.SitePrefixID, blocks[0].SitePrefixID)
			require.Equal(t, root.Name, blocks[0].Name)
		}},
		{"operator reactivation preserves root identity and IPAM", func(t *testing.T, f fixture) {
			root := f.root(t, true)
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			ready := testInventory(prefix)
			deleting := proto.Clone(ready).(*corev1.SitePrefixInventory)
			deleting.SitePrefixes[0].Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			deleting.Timestamp = timestamppb.New(ready.Timestamp.AsTime().Add(time.Second))
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, deleting))
			block := f.blocks(t)[0]
			require.Equal(t, cdbm.IPBlockStatusDeleting, block.Status)
			ready.Timestamp = timestamppb.New(deleting.Timestamp.AsTime().Add(time.Second))
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, ready))
			block = f.blocks(t)[0]
			require.Equal(t, cdbm.IPBlockStatusReady, block.Status)
			require.Equal(t, root.ID, block.ID)
			require.Equal(t, root.SitePrefixID, block.SitePrefixID)
			prefixes, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, f.namespace())
			require.NoError(t, err)
			require.Equal(t, []string{prefix.Config.Prefix}, prefixes)
		}},
		{"replay is idempotent and changed reports reconcile", func(t *testing.T, f fixture) {
			inventory := testInventory(testPrefix())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			site, block := f.inventorySite(t), f.blocks(t)[0]
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, proto.Clone(inventory).(*corev1.SitePrefixInventory)))
			require.Equal(t, site.Updated, f.inventorySite(t).Updated)
			require.Equal(t, block, f.blocks(t)[0])
			details, _, err := cdbm.NewStatusDetailDAO(f.session).GetAll(ctx, nil,
				cdbm.StatusDetailFilterInput{EntityIDs: []string{block.ID.String()}}, paginator.PageInput{})
			require.NoError(t, err)
			require.Len(t, details, 1)
			inventory.SitePrefixes[0].Metadata.Name = "changed"
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Equal(t, block, f.blocks(t)[0])
		}},
		{"empty inventory does not infer absence", func(t *testing.T, f fixture) {
			inventory := testInventory(testPrefix())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			block := f.blocks(t)[0]
			inventory.SitePrefixes, inventory.InventoryPage.ItemIds = nil, nil
			inventory.InventoryPage.TotalItems, inventory.InventoryPage.TotalPages = 0, 0
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Equal(t, []cdbm.IPBlock{block}, f.blocks(t))
		}},
		{"late final page is reconciled without discarding earlier resources", func(t *testing.T, f fixture) {
			ids := []string{uuid.NewString(), uuid.NewString()}
			slices.Sort(ids)
			first := testInventory(testPrefix())
			first.Timestamp = timestamppb.New(time.Now().Add(-4 * time.Minute))
			first.SitePrefixes[0].Id.Value = ids[0]
			first.InventoryPage = &corev1.InventoryPage{CurrentPage: 1, TotalPages: 2, TotalItems: 2, PageSize: 1, ItemIds: ids}
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, first))
			final := proto.Clone(first).(*corev1.SitePrefixInventory)
			final.InventoryPage.CurrentPage = 2
			final.SitePrefixes[0].Id.Value = ids[1]
			final.SitePrefixes[0].Config.Prefix = "10.1.0.0/24"
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, final))
			block, err := cdbm.NewIPBlockDAO(f.session).GetBySitePrefixID(ctx, nil, uuid.MustParse(ids[1]))
			require.NoError(t, err)
			require.Equal(t, "10.1.0.0", block.Prefix)
			require.Len(t, f.blocks(t), 2)
		}},
		{"failure diagnostics do not change the Site or resources", func(t *testing.T, f fixture) {
			inventory := testInventory(testPrefix())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			site := f.inventorySite(t)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, &corev1.SitePrefixInventory{
				InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED, StatusMsg: "Core unavailable",
				SitePrefixes: []*corev1.SitePrefix{nil},
			}))
			require.Equal(t, site, f.inventorySite(t))
			require.Len(t, f.blocks(t), 1)
		}},
		{"later unknown tenant preserves earlier resource and retries converge", func(t *testing.T, f fixture) {
			first, second := testPrefix(), testPrefix()
			ids := []string{first.Id.Value, second.Id.Value}
			slices.Sort(ids)
			first.Id.Value, second.Id.Value = ids[0], ids[1]
			second.Config.TenantOrganizationId = cutil.GetPtr("unknown")
			second.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
			inventory := testInventory(first)
			inventory.SitePrefixes = append(inventory.SitePrefixes, second)
			inventory.InventoryPage.ItemIds, inventory.InventoryPage.TotalItems = ids, 2
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			firstBlock := blocks[0]
			require.Equal(t, first.Id.Value, firstBlock.SitePrefixID.String())
			prefixes, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixes(ctx, f.namespace())
			require.NoError(t, err)
			require.Len(t, prefixes, 1)
			util.TestBuildTenant(t, f.session, "unknown", "unknown", nil, &cdbm.User{ID: f.site.CreatedBy})
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Len(t, f.blocks(t), 2)
			retained, err := cdbm.NewIPBlockDAO(f.session).GetByID(ctx, nil, firstBlock.ID, nil)
			require.NoError(t, err)
			require.Equal(t, firstBlock, *retained)
		}},
		{"later invalid prefix preserves earlier resource", func(t *testing.T, f fixture) {
			first, second := testPrefix(), testPrefix()
			ids := []string{first.Id.Value, second.Id.Value}
			slices.Sort(ids)
			first.Id.Value, second.Id.Value = ids[0], ids[1]
			second.Config.Prefix = "10.1.0.1/24"
			inventory := testInventory(first)
			inventory.SitePrefixes = append(inventory.SitePrefixes, second)
			inventory.InventoryPage.ItemIds, inventory.InventoryPage.TotalItems = ids, 2
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			require.Equal(t, first.Id.Value, blocks[0].SitePrefixID.String())
			prefixes, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, f.namespace())
			require.NoError(t, err)
			require.Equal(t, []string{first.Config.Prefix}, prefixes)
		}},
		{"failed prefix rolls back its IPAM and record without stopping later entries", func(t *testing.T, f fixture) {
			first, second := testPrefix(), testPrefix()
			ids := []string{first.Id.Value, second.Id.Value}
			slices.Sort(ids)
			first.Id.Value, second.Id.Value = ids[0], ids[1]
			second.Config.Prefix = "10.1.0.0/24"
			inventory := testInventory(first)
			third := testPrefix()
			third.Config.Prefix = "10.2.0.0/24"
			inventory.SitePrefixes = append(inventory.SitePrefixes, second, third)
			inventory.InventoryPage.ItemIds, inventory.InventoryPage.TotalItems = ids, 2
			f.session.DB.AddQueryHook(&statusDetailFailureHook{remaining: 2})
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			blocks := f.blocks(t)
			require.Len(t, blocks, 2)
			firstBlock := blocks[0]
			require.Equal(t, first.Id.Value, firstBlock.SitePrefixID.String())
			prefixes, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, f.namespace())
			require.NoError(t, err)
			require.ElementsMatch(t, []string{first.Config.Prefix, third.Config.Prefix}, prefixes)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Len(t, f.blocks(t), 3)
			retained, err := cdbm.NewIPBlockDAO(f.session).GetByID(ctx, nil, firstBlock.ID, nil)
			require.NoError(t, err)
			require.Equal(t, firstBlock, *retained)
			prefixes, err = ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, f.namespace())
			require.NoError(t, err)
			require.ElementsMatch(t, []string{first.Config.Prefix, second.Config.Prefix, third.Config.Prefix}, prefixes)
		}},
		{"retired operator Deleting reports are skipped without stopping the page", func(t *testing.T, f fixture) {
			root := f.retiredRoot(t)
			prefix, next := testPrefix(), testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			prefix.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			next.Config.Prefix = "10.1.0.0/24"
			inventory := testInventory(prefix)
			inventory.SitePrefixes = append(inventory.SitePrefixes, next)
			for range 2 {
				require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			}
			retained, err := cdbm.NewIPBlockDAO(f.session).GetBySitePrefixID(ctx, nil, *root.SitePrefixID)
			require.NoError(t, err)
			require.Equal(t, root, retained)
			require.Len(t, f.blocks(t), 2)
			prefixes, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, f.namespace())
			require.NoError(t, err)
			require.Equal(t, []string{next.Config.Prefix}, prefixes)
			details, _, err := cdbm.NewStatusDetailDAO(f.session).GetAll(ctx, nil,
				cdbm.StatusDetailFilterInput{EntityIDs: []string{root.ID.String()}}, paginator.PageInput{})
			require.NoError(t, err)
			require.Empty(t, details)
		}},
		{"retired operator Ready restores its identity and IPAM idempotently", func(t *testing.T, f fixture) {
			root := f.retiredRoot(t)
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			blocks := f.blocks(t)
			require.Len(t, blocks, 1)
			restored := blocks[0]
			require.Equal(t, root.ID, restored.ID)
			require.Equal(t, root.SitePrefixID, restored.SitePrefixID)
			require.Equal(t, root.Created, restored.Created)
			require.Nil(t, restored.Deleted)
			require.Equal(t, cdbm.IPBlockStatusReady, restored.Status)
			require.Equal(t, root.Name, restored.Name)
			require.Equal(t, root.Description, restored.Description)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			require.Equal(t, restored, f.blocks(t)[0])
			prefixes, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, f.namespace())
			require.NoError(t, err)
			require.Equal(t, []string{prefix.Config.Prefix}, prefixes)
			details, _, err := cdbm.NewStatusDetailDAO(f.session).GetAll(ctx, nil,
				cdbm.StatusDetailFilterInput{EntityIDs: []string{root.ID.String()}}, paginator.PageInput{})
			require.NoError(t, err)
			require.Len(t, details, 1)
			require.Equal(t, cdbm.IPBlockStatusReady, details[0].Status)
		}},
		{"failed operator restoration rolls back the link IPAM and status", func(t *testing.T, f fixture) {
			root := f.retiredRoot(t)
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			f.session.DB.AddQueryHook(&statusDetailFailureHook{remaining: 1})
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			require.Equal(t, *root, f.blocks(t)[0])
			prefixes, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, f.namespace())
			require.NoError(t, err)
			require.Empty(t, prefixes)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			restored := f.blocks(t)[0]
			require.Equal(t, root.ID, restored.ID)
			require.Nil(t, restored.Deleted)
			require.Equal(t, cdbm.IPBlockStatusReady, restored.Status)
			prefixes, err = ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, f.namespace())
			require.NoError(t, err)
			require.Equal(t, []string{prefix.Config.Prefix}, prefixes)
		}},
		{"IPAM conflict leaves the retired operator and existing tree unchanged", func(t *testing.T, f fixture) {
			root := f.retiredRoot(t)
			storage := ipam.NewIpamStorage(f.session.DB, nil)
			_, err := ipam.CreateIpamEntryForIPBlock(ctx, storage, "10.0.0.0", 23,
				root.RoutingType, root.InfrastructureProviderID.String(), root.SiteID.String())
			require.NoError(t, err)
			before, err := storage.ReadAllPrefixes(ctx, f.namespace())
			require.NoError(t, err)
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			require.Equal(t, *root, f.blocks(t)[0])
			after, err := storage.ReadAllPrefixes(ctx, f.namespace())
			require.NoError(t, err)
			require.Equal(t, before, after)
		}},
		{"soft deleted tenant Core ID cannot be reused", func(t *testing.T, f fixture) {
			prefix := testPrefix()
			prefix.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
			prefix.Config.TenantOrganizationId = &f.tenant.Org
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			root := f.blocks(t)[0]
			require.NoError(t, cdbm.NewIPBlockDAO(f.session).Delete(ctx, nil, root.ID))
			prefix.Id.Value = root.SitePrefixID.String()
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
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
					require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
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
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Empty(t, f.blocks(t))
			require.NoError(t, tx.Commit())
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			require.Len(t, f.blocks(t), 1)
		}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) { tt.check(t, setup(t)) })
	}
	for _, cidr := range []string{"10.0.0.0/24", "10.0.0.0/23"} {
		t.Run("operator restoration waits for conflicting "+cidr, func(t *testing.T) {
			f := setup(t)
			root := f.retiredRoot(t)
			replacement := testPrefix()
			replacement.Config.Prefix = cidr
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(replacement)))
			before := f.blocks(t)
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			require.ElementsMatch(t, before, f.blocks(t))
			prefixes, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, f.namespace())
			require.NoError(t, err)
			require.Equal(t, []string{cidr}, prefixes)
			replacementBlock, err := cdbm.NewIPBlockDAO(f.session).GetBySitePrefixID(ctx, nil, uuid.MustParse(replacement.Id.Value))
			require.NoError(t, err)
			err = cdb.WithTx(ctx, f.session, func(tx *cdb.Tx) error {
				err := cdbm.NewIPBlockDAO(f.session).Delete(ctx, tx, replacementBlock.ID)
				if err != nil {
					return err
				}
				return ipam.DeleteIpamEntryForIPBlock(ctx, ipam.NewIpamStorage(f.session.DB, tx.GetBunTx()),
					replacementBlock.Prefix, replacementBlock.PrefixLength, replacementBlock.RoutingType,
					replacementBlock.InfrastructureProviderID.String(), replacementBlock.SiteID.String())
			})
			require.NoError(t, err)
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			restored, err := cdbm.NewIPBlockDAO(f.session).GetByID(ctx, nil, root.ID, nil)
			require.NoError(t, err)
			require.Equal(t, root.SitePrefixID, restored.SitePrefixID)
			require.Equal(t, cdbm.IPBlockStatusReady, restored.Status)
			prefixes, err = ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, f.namespace())
			require.NoError(t, err)
			require.Equal(t, []string{prefix.Config.Prefix}, prefixes)
		})
	}
	for _, state := range []corev1.SitePrefixLifecycleState{
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING,
		corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY,
	} {
		t.Run("retired operator rejects changed identity before "+state.String(), func(t *testing.T) {
			f := setup(t)
			root := f.retiredRoot(t)
			prefix := testPrefix()
			prefix.Id.Value = root.SitePrefixID.String()
			prefix.Config.Prefix = "10.0.0.0/23"
			prefix.Status.LifecycleState = state
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			require.Equal(t, *root, f.blocks(t)[0])
		})
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
			tx, err := cdb.BeginTx(ctx, f.session, nil)
			require.NoError(t, err)
			defer func() { _ = tx.Rollback() }()
			require.ErrorContains(t, ipam.LockAndValidateParentIPBlockForAllocation(ctx, tx, f.session, root), "not Ready")
		})
	}

	for _, change := range []struct {
		name  string
		apply func(*cdbm.IPBlock)
	}{
		{"routing mismatch", func(b *cdbm.IPBlock) { b.RoutingType = cdbm.IPBlockRoutingTypePublic }},
		{"family mismatch", func(b *cdbm.IPBlock) { b.ProtocolVersion = cdbm.IPBlockProtocolVersionV6 }},
		{"overlapping root", func(b *cdbm.IPBlock) { b.PrefixLength = 16 }},
		{"ambiguous roots", func(_ *cdbm.IPBlock) {}},
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
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(testPrefix())))
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
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(prefix)))
			require.Equal(t, root.Name, f.blocks(t)[0].Name)
		})
	}
}

// Delete after the first prefix commits, before the second locks the Site.
type siteDeletionHook struct {
	session *cdb.Session
	siteID  uuid.UUID
	reads   int
	err     error
}

func (hook *siteDeletionHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if strings.Contains(event.Query, "FROM \"site\" AS \"st\"") && strings.Contains(event.Query, "FOR NO KEY UPDATE") {
		hook.reads++
		if hook.reads == 2 {
			hook.err = cdbm.NewSiteDAO(hook.session).Delete(ctx, nil, hook.siteID)
		}
	}
	return ctx
}

func (*siteDeletionHook) AfterQuery(context.Context, *bun.QueryEvent) {}

// Fail one status-detail insert after the prefix's REST and IPAM writes.
type statusDetailFailureHook struct {
	remaining int
}

func (hook *statusDetailFailureHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if strings.HasPrefix(event.Query, "INSERT INTO \"status_detail\"") {
		hook.remaining--
		if hook.remaining == 0 {
			failedCtx, cancel := context.WithCancel(ctx)
			cancel()
			return failedCtx
		}
	}
	return ctx
}

func (*statusDetailFailureHook) AfterQuery(context.Context, *bun.QueryEvent) {}

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
		require.NoError(t, <-done)
		require.Empty(t, f.blocks(t))
	}
	require.ErrorIs(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory), cdb.ErrDoesNotExist)
	deletedSite, err := dao.GetByID(ctx, nil, f.site.ID, nil, true)
	require.NoError(t, err)
	require.NotNil(t, deletedSite.Deleted)
}

func testPrefix() *corev1.SitePrefix {
	return &corev1.SitePrefix{
		Id: &corev1.SitePrefixId{Value: uuid.NewString()},
		Config: &corev1.SitePrefixConfig{Prefix: "10.0.0.0/24",
			RoutingScope: corev1.SitePrefixRoutingScope_SITE_PREFIX_ROUTING_SCOPE_DATACENTER_ONLY},
		Status: &corev1.SitePrefixStatus{
			Authority:      corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_OPERATOR_MANAGED,
			LifecycleState: corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY},
		Metadata: &corev1.Metadata{Name: "test-prefix", Description: "reported by Core"},
	}
}

func testInventory(prefix *corev1.SitePrefix) *corev1.SitePrefixInventory {
	return &corev1.SitePrefixInventory{
		Timestamp: timestamppb.Now(), InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		InventoryPage: &corev1.InventoryPage{CurrentPage: 1, TotalPages: 1, TotalItems: 1, PageSize: 25,
			ItemIds: []string{prefix.Id.Value}},
		SitePrefixes: []*corev1.SitePrefix{prefix},
	}
}

func TestValidatePrefix(t *testing.T) {
	type testCase struct {
		name   string
		change func(*corev1.SitePrefix) *corev1.SitePrefix
		valid  bool
	}
	tests := []testCase{
		{"operator", func(p *corev1.SitePrefix) *corev1.SitePrefix { return p }, true},
		{"missing prefix", func(_ *corev1.SitePrefix) *corev1.SitePrefix { return nil }, false},
		{"missing ID", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Id = nil; return p }, false},
		{"invalid ID", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Id.Value = "invalid"; return p }, false},
		{"nil UUID", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Id.Value = uuid.Nil.String(); return p }, false},
		{"noncanonical ID", func(p *corev1.SitePrefix) *corev1.SitePrefix {
			p.Id.Value = "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA"
			return p
		}, false},
		{"missing config", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Config = nil; return p }, false},
		{"missing status", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Status = nil; return p }, false},
		{"unspecified authority", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Status.Authority = 0; return p }, false},
		{"unspecified state", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Status.LifecycleState = 0; return p }, false},
		{"operator provisioning", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Status.LifecycleState = 1; return p }, false},
		{"operator owner", func(p *corev1.SitePrefix) *corev1.SitePrefix {
			p.Config.TenantOrganizationId = cutil.GetPtr("tenant")
			return p
		}, false},
		{"unsupported routing", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Config.RoutingScope = 0; return p }, false},
		{"host bits", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Config.Prefix = "10.0.0.1/24"; return p }, false},
		{"operator v6", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Config.Prefix = "fd00::/64"; return p }, true},
		{"metadata absent", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Metadata = nil; return p }, false},
		{"metadata bounds", func(p *corev1.SitePrefix) *corev1.SitePrefix { p.Metadata.Name = strings.Repeat("x", 257); return p }, false},
		{"duplicate label", func(p *corev1.SitePrefix) *corev1.SitePrefix {
			p.Metadata.Labels = []*corev1.Label{{Key: "x"}, {Key: "x"}}
			return p
		}, false},
	}
	for _, tt := range []struct {
		cidr  string
		valid bool
	}{
		{"10.0.0.0/8", true}, {"172.16.0.0/12", true}, {"192.168.0.0/31", true},
		{"10.0.0.0/7", false}, {"10.0.0.0/32", false}, {"192.168.0.0/15", false},
		{"100.64.0.0/10", false}, {"203.0.113.0/24", false}, {"fd00::/64", false},
	} {
		tests = append(tests, testCase{"tenant " + tt.cidr, func(p *corev1.SitePrefix) *corev1.SitePrefix {
			p.Status.Authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
			p.Config.TenantOrganizationId = cutil.GetPtr("tenant")
			p.Config.Prefix = tt.cidr
			return p
		}, tt.valid})
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := validatePrefix(tt.change(testPrefix()))
			if tt.valid {
				require.NoError(t, err)
			} else {
				var applicationErr *temporal.ApplicationError
				require.ErrorAs(t, err, &applicationErr)
				require.True(t, applicationErr.NonRetryable())
			}
		})
	}
}
