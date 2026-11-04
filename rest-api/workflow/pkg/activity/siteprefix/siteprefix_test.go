// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"bytes"
	"context"
	"fmt"
	"net/netip"
	"reflect"
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
	"google.golang.org/protobuf/types/known/timestamppb"
)

// testSitePrefix describes a SitePrefix that Core reports. A zero state is Ready.
type testSitePrefix struct {
	id            string
	cidr          string
	name          string
	description   string
	state         corev1.SitePrefixLifecycleState
	tenantManaged bool
	tenantOrg     string
	labels        []*corev1.Label
	// The fields below build SitePrefixes that only validation should accept or reject.
	nilEntry                bool
	omitConfig              bool
	omitStatus              bool
	omitMetadata            bool
	unspecifiedAuthority    bool
	unspecifiedState        bool
	unspecifiedRoutingScope bool
}

func (p testSitePrefix) toProto() *corev1.SitePrefix {
	if p.nilEntry {
		return nil
	}
	state := p.state
	if state == corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_UNSPECIFIED {
		state = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_READY
	}
	if p.unspecifiedState {
		state = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_UNSPECIFIED
	}
	authority := corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_OPERATOR_MANAGED
	if p.tenantManaged {
		authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_TENANT_MANAGED
	}
	if p.unspecifiedAuthority {
		authority = corev1.SitePrefixAuthority_SITE_PREFIX_AUTHORITY_UNSPECIFIED
	}
	routingScope := corev1.SitePrefixRoutingScope_SITE_PREFIX_ROUTING_SCOPE_DATACENTER_ONLY
	if p.unspecifiedRoutingScope {
		routingScope = corev1.SitePrefixRoutingScope_SITE_PREFIX_ROUTING_SCOPE_UNSPECIFIED
	}
	var tenantOrg *string
	if p.tenantOrg != "" {
		tenantOrg = cutil.GetPtr(p.tenantOrg)
	}

	prefix := &corev1.SitePrefix{Id: &corev1.SitePrefixId{Value: p.id}}
	if !p.omitConfig {
		prefix.Config = &corev1.SitePrefixConfig{
			Prefix:               p.cidr,
			TenantOrganizationId: tenantOrg,
			RoutingScope:         routingScope,
		}
	}
	if !p.omitStatus {
		prefix.Status = &corev1.SitePrefixStatus{
			Authority:      authority,
			LifecycleState: state,
		}
	}
	if !p.omitMetadata {
		prefix.Metadata = &corev1.Metadata{
			Name:        p.name,
			Description: p.description,
			Labels:      p.labels,
		}
	}
	return prefix
}

// testSitePrefixInventory returns a successful single-page inventory of prefixes.
func testSitePrefixInventory(prefixes []testSitePrefix) *corev1.SitePrefixInventory {
	inventory := &corev1.SitePrefixInventory{
		Timestamp:       timestamppb.Now(),
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		InventoryPage: &corev1.InventoryPage{
			CurrentPage: 1,
			TotalPages:  1,
			TotalItems:  int32(len(prefixes)),
			PageSize:    25,
		},
	}
	for _, prefix := range prefixes {
		inventory.SitePrefixes = append(inventory.SitePrefixes, prefix.toProto())
		inventory.InventoryPage.ItemIds = append(inventory.InventoryPage.ItemIds, prefix.id)
	}
	return inventory
}

// testExistingIPBlock is an IP Block that exists before the inventory under test.
type testExistingIPBlock struct {
	cidr        string
	name        string
	description string
	// public gives the IP Block the Public routing type instead of DatacenterOnly.
	public bool
	// sitePrefixID links the IP Block to a Core SitePrefix. Empty leaves it unlinked.
	sitePrefixID string
	// tenantOrg makes it a Tenant-created IP Block, which has no IPAM entry.
	tenantOrg string
	// otherSite creates the IP Block on another Provider's Site.
	otherSite bool
	// allocatedChild allocates a /28 from the IP Block in IPAM.
	allocatedChild bool
	// retired soft-deletes the IP Block deletedAgo before the call under test and
	// removes its IPAM entry.
	retired    bool
	deletedAgo time.Duration
}

// testIPBlockState is the state of an IP Block after the call under test.
type testIPBlockState struct {
	cidr         string
	sitePrefixID string
	name         string
	description  *string
	status       string
	public       bool
	tenantOrg    string
	otherSite    bool
	deleted      bool
	// existing means the IP Block existed before the call under test.
	existing bool
	// unchanged means the call under test did not write the IP Block.
	unchanged     bool
	statusDetails int
}

// testConcurrentWriter is a write that races UpdateSitePrefixesInDB.
type testConcurrentWriter int

const (
	// testSiteDeletionFirst deletes the Site in a transaction that commits while
	// the inventory waits for the Site row.
	testSiteDeletionFirst testConcurrentWriter = iota + 1
	// testSiteDeletionWaiting deletes the Site while the inventory holds the Site row.
	testSiteDeletionWaiting
	// testSiteDeletionBetweenPrefixes deletes the Site after the first prefix commits.
	testSiteDeletionBetweenPrefixes
	// testAllocationWaiting creates an Allocation's child IP Block while the
	// inventory holds the Site row and waits for the Allocation's root.
	testAllocationWaiting
)

type testFixture struct {
	session   *cdb.Session
	user      *cdbm.User
	site      *cdbm.Site
	otherSite *cdbm.Site
	// tenants are keyed by org.
	tenants map[string]*cdbm.Tenant
	manager ManageSitePrefix
}

func newTestFixture(t *testing.T) testFixture {
	t.Helper()
	session := util.TestInitDB(t)
	t.Cleanup(session.Close)
	util.TestSetupSchema(t, session)
	storage := cipam.NewBunStorage(session.DB, nil)
	require.NoError(t, storage.ApplyDbSchema())
	require.NoError(t, storage.DeleteAllPrefixesFromAllNamespaces(context.Background()))
	user := util.TestBuildUser(t, session, uuid.NewString(), []string{"provider"}, []string{"FORGE_PROVIDER_ADMIN"})
	provider := util.TestBuildInfrastructureProvider(t, session, "provider", "provider", user)
	otherProvider := util.TestBuildInfrastructureProvider(t, session, "other-provider", "other-provider", user)
	return testFixture{
		session:   session,
		user:      user,
		site:      util.TestBuildSite(t, session, provider, "site", cdbm.SiteStatusRegistered, nil, user),
		otherSite: util.TestBuildSite(t, session, otherProvider, "other-site", cdbm.SiteStatusRegistered, nil, user),
		tenants: map[string]*cdbm.Tenant{
			"tenant": util.TestBuildTenant(t, session, "tenant", "tenant", nil, user),
		},
		manager: NewManageSitePrefix(session),
	}
}

func (f testFixture) addTenants(t *testing.T, orgs []string) {
	t.Helper()
	for _, org := range orgs {
		f.tenants[org] = util.TestBuildTenant(t, f.session, org, org, nil, f.user)
	}
}

func (f testFixture) createExistingIPBlocks(t *testing.T, blocks []testExistingIPBlock) {
	t.Helper()
	ctx := context.Background()
	dao := cdbm.NewIPBlockDAO(f.session)
	storage := ipam.NewIpamStorage(f.session.DB, nil)
	for _, block := range blocks {
		prefix := netip.MustParsePrefix(block.cidr)
		site := f.site
		if block.otherSite {
			site = f.otherSite
		}
		routingType := cdbm.IPBlockRoutingTypeDatacenterOnly
		if block.public {
			routingType = cdbm.IPBlockRoutingTypePublic
		}
		protocolVersion := cdbm.IPBlockProtocolVersionV4
		if !prefix.Addr().Is4() {
			protocolVersion = cdbm.IPBlockProtocolVersionV6
		}
		var description *string
		if block.description != "" {
			description = cutil.GetPtr(block.description)
		}
		var sitePrefixID *uuid.UUID
		if block.sitePrefixID != "" {
			sitePrefixID = cutil.GetPtr(uuid.MustParse(block.sitePrefixID))
		}
		var tenantID *uuid.UUID
		if block.tenantOrg != "" {
			tenantID = &f.tenants[block.tenantOrg].ID
		}
		created, err := dao.Create(ctx, nil, cdbm.IPBlockCreateInput{
			Name:                     block.name,
			Description:              description,
			SiteID:                   site.ID,
			InfrastructureProviderID: site.InfrastructureProviderID,
			TenantID:                 tenantID,
			SitePrefixID:             sitePrefixID,
			Managed:                  cutil.GetPtr(block.tenantOrg == ""),
			RoutingType:              routingType,
			Prefix:                   prefix.Addr().String(),
			PrefixLength:             prefix.Bits(),
			ProtocolVersion:          protocolVersion,
			Status:                   cdbm.IPBlockStatusReady,
			CreatedBy:                &f.user.ID,
		})
		require.NoError(t, err)

		withIPAM := block.tenantOrg == ""
		if withIPAM {
			_, err = ipam.CreateIpamEntryForIPBlock(ctx, storage, created.Prefix, created.PrefixLength,
				created.RoutingType, created.InfrastructureProviderID.String(), created.SiteID.String())
			require.NoError(t, err)
		}
		if block.allocatedChild {
			allocator := cipam.NewWithStorage(storage)
			allocator.SetNamespace(ipam.GetIpamNamespaceForIPBlock(ctx, created.RoutingType,
				created.InfrastructureProviderID.String(), created.SiteID.String()))
			_, err = allocator.AcquireChildPrefix(ctx, block.cidr, 28)
			require.NoError(t, err)
		}
		if !block.retired {
			continue
		}
		_, err = dao.Update(ctx, nil, cdbm.IPBlockUpdateInput{
			IPBlockID: created.ID,
			Status:    cutil.GetPtr(cdbm.IPBlockStatusDeleting),
		})
		require.NoError(t, err)
		require.NoError(t, dao.Delete(ctx, nil, created.ID))
		if withIPAM {
			require.NoError(t, ipam.DeleteIpamEntryForIPBlock(ctx, storage, created.Prefix, created.PrefixLength,
				created.RoutingType, created.InfrastructureProviderID.String(), created.SiteID.String()))
		}
		if block.deletedAgo > 0 {
			_, err = f.session.DB.NewUpdate().Model((*cdbm.IPBlock)(nil)).WhereAllWithDeleted().
				Set("deleted = ?", time.Now().Add(-block.deletedAgo)).Where("id = ?", created.ID).Exec(ctx)
			require.NoError(t, err)
		}
	}
}

// blocks returns every IP Block, including soft-deleted ones.
func (f testFixture) blocks(t *testing.T) []cdbm.IPBlock {
	t.Helper()
	blocks, _, err := cdbm.NewIPBlockDAO(f.session).GetAll(context.Background(), nil,
		cdbm.IPBlockFilterInput{IncludeDeleted: true},
		paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	require.NoError(t, err)
	return blocks
}

func (f testFixture) blocksByID(t *testing.T) map[uuid.UUID]cdbm.IPBlock {
	t.Helper()
	blocks := map[uuid.UUID]cdbm.IPBlock{}
	for _, block := range f.blocks(t) {
		blocks[block.ID] = block
	}
	return blocks
}

// ipBlockStates describes every IP Block against before, the IP Blocks that existed
// before the call under test.
func (f testFixture) ipBlockStates(t *testing.T, before map[uuid.UUID]cdbm.IPBlock) []testIPBlockState {
	t.Helper()
	orgs := map[uuid.UUID]string{}
	for org, tenant := range f.tenants {
		orgs[tenant.ID] = org
	}
	statusDetailDAO := cdbm.NewStatusDetailDAO(f.session)
	states := []testIPBlockState{}
	for _, block := range f.blocks(t) {
		require.Equal(t, block.SitePrefixID == nil || block.TenantID == nil, block.Managed, "managed flag of %s", block.Name)
		previous, existing := before[block.ID]
		state := testIPBlockState{
			cidr:        fmt.Sprintf("%s/%d", block.Prefix, block.PrefixLength),
			name:        block.Name,
			description: block.Description,
			status:      block.Status,
			public:      block.RoutingType == cdbm.IPBlockRoutingTypePublic,
			otherSite:   block.SiteID == f.otherSite.ID,
			deleted:     block.Deleted != nil,
			existing:    existing,
			unchanged:   existing && reflect.DeepEqual(previous, block),
		}
		if block.SitePrefixID != nil {
			state.sitePrefixID = block.SitePrefixID.String()
		}
		if block.TenantID != nil {
			state.tenantOrg = orgs[*block.TenantID]
		}
		details, _, err := statusDetailDAO.GetAll(context.Background(), nil,
			cdbm.StatusDetailFilterInput{EntityIDs: []string{block.ID.String()}}, paginator.PageInput{})
		require.NoError(t, err)
		state.statusDetails = len(details)
		states = append(states, state)
	}
	return states
}

func (f testFixture) linkedBlock(t *testing.T, sitePrefixID string) *cdbm.IPBlock {
	t.Helper()
	blocks, _, err := cdbm.NewIPBlockDAO(f.session).GetAll(context.Background(), nil,
		cdbm.IPBlockFilterInput{SitePrefixIDs: []uuid.UUID{uuid.MustParse(sitePrefixID)}},
		paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	require.NoError(t, err)
	require.Len(t, blocks, 1)
	return &blocks[0]
}

// ipamCIDRs returns the CIDRs in the Site's IPAM namespace for routingType.
func (f testFixture) ipamCIDRs(t *testing.T, routingType string) []string {
	t.Helper()
	ctx := context.Background()
	namespace := ipam.GetIpamNamespaceForIPBlock(ctx, routingType,
		f.site.InfrastructureProviderID.String(), f.site.ID.String())
	cidrs, err := ipam.NewIpamStorage(f.session.DB, nil).ReadAllPrefixCidrs(ctx, namespace)
	require.NoError(t, err)
	return cidrs
}

// currentSite returns the Site, including after it is deleted.
func (f testFixture) currentSite(t *testing.T) *cdbm.Site {
	t.Helper()
	site := &cdbm.Site{}
	err := f.session.DB.NewSelect().Model(site).Where("st.id = ?", f.site.ID).WhereAllWithDeleted().Scan(context.Background())
	require.NoError(t, err)
	return site
}

// runWithSiteDeletionFirst runs the inventory while a Site deletion holds the
// Site row, then commits the deletion once the inventory waits for it.
func (f testFixture) runWithSiteDeletionFirst(t *testing.T, inventory *corev1.SitePrefixInventory) error {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	tx, err := cdb.BeginTx(ctx, f.session, nil)
	require.NoError(t, err)
	defer func() { _ = tx.Rollback() }()
	var writerPID int
	require.NoError(t, tx.GetBunTx().NewSelect().ColumnExpr("pg_backend_pid()").Scan(ctx, &writerPID))
	require.NoError(t, cdbm.NewSiteDAO(f.session).Delete(ctx, tx, f.site.ID))

	done := make(chan error, 1)
	go func() { done <- f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory) }()
	require.Eventually(t, func() bool {
		var waiters int
		err := f.session.DB.NewSelect().ColumnExpr("count(*)").TableExpr("pg_catalog.pg_stat_activity").
			Where("? = ANY(pg_blocking_pids(pid))", writerPID).Scan(ctx, &waiters)
		return err == nil && waiters > 0
	}, 5*time.Second, 10*time.Millisecond, "inventory must reload the Site under its row lock")
	require.NoError(t, tx.Commit())
	return <-done
}

// runWithSiteDeletionWaiting pauses the inventory once it holds the Site row,
// starts a Site deletion that has to wait for it, then lets the inventory finish.
func (f testFixture) runWithSiteDeletionWaiting(t *testing.T, inventory *corev1.SitePrefixInventory) error {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	hook := &testSiteLockHook{
		entered: make(chan struct{}),
		resume:  make(chan struct{}),
	}
	f.session.DB.AddQueryHook(hook)
	var release sync.Once
	defer release.Do(func() { close(hook.resume) })

	done := make(chan error, 1)
	go func() { done <- f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory) }()
	select {
	case <-hook.entered:
	case <-ctx.Done():
		t.Fatal("inventory did not lock the Site")
	}
	deleted := make(chan error, 1)
	go func() { deleted <- cdbm.NewSiteDAO(f.session).Delete(ctx, nil, f.site.ID) }()
	require.Eventually(t, func() bool {
		var waiters int
		err := f.session.DB.NewSelect().ColumnExpr("count(*)").TableExpr("pg_catalog.pg_stat_activity").
			Where("datname = current_database() AND cardinality(pg_blocking_pids(pid)) > 0").
			Where("query LIKE ?", "%\"site\"%").Scan(ctx, &waiters)
		return err == nil && waiters > 0
	}, 5*time.Second, 10*time.Millisecond, "Site deletion must wait for the inventory")
	release.Do(func() { close(hook.resume) })
	err := <-done
	require.NoError(t, <-deleted)
	return err
}

// runWithSiteDeletionBetweenPrefixes deletes the Site after the first prefix
// commits and before the second one locks the Site.
func (f testFixture) runWithSiteDeletionBetweenPrefixes(t *testing.T, inventory *corev1.SitePrefixInventory) error {
	t.Helper()
	hook := &testSiteDeletionHook{
		session: f.session,
		siteID:  f.site.ID,
	}
	f.session.DB.AddQueryHook(hook)
	err := f.manager.UpdateSitePrefixesInDB(context.Background(), f.site.ID, inventory)
	require.NoError(t, hook.err)
	require.Equal(t, 2, hook.reads)
	return err
}

// runWithAllocationWaiting locks the first reported root for an Allocation and
// waits for the inventory to queue behind it while holding the Site row. The
// Allocation's child IP Block still has to pass its Site foreign key check.
func (f testFixture) runWithAllocationWaiting(t *testing.T, inventory *corev1.SitePrefixInventory) error {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	root := f.linkedBlock(t, inventory.SitePrefixes[0].GetId().GetValue())
	tx, err := cdb.BeginTx(ctx, f.session, nil)
	require.NoError(t, err)
	defer func() { _ = tx.Rollback() }()
	require.NoError(t, ipam.LockAndValidateParentIPBlockForAllocation(ctx, tx, f.session, root))
	var allocatorPID int
	require.NoError(t, tx.GetBunTx().NewSelect().ColumnExpr("pg_backend_pid()").Scan(ctx, &allocatorPID))

	done := make(chan error, 1)
	go func() { done <- f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory) }()
	require.Eventually(t, func() bool {
		var waiters int
		err := f.session.DB.NewSelect().ColumnExpr("count(*)").TableExpr("pg_catalog.pg_stat_activity").
			Where("? = ANY(pg_blocking_pids(pid))", allocatorPID).
			Where("query LIKE ?", "%\"ip_block\"%FOR UPDATE%").Scan(ctx, &waiters)
		return err == nil && waiters > 0
	}, 5*time.Second, 10*time.Millisecond, "inventory must hold the Site while waiting for the Allocation's root")
	_, err = cdbm.NewIPBlockDAO(f.session).Create(ctx, tx, cdbm.IPBlockCreateInput{
		Name:                     "allocated-child",
		SiteID:                   f.site.ID,
		InfrastructureProviderID: f.site.InfrastructureProviderID,
		TenantID:                 &f.tenants["tenant"].ID,
		RoutingType:              root.RoutingType,
		Prefix:                   root.Prefix,
		PrefixLength:             25,
		ProtocolVersion:          root.ProtocolVersion,
		Status:                   cdbm.IPBlockStatusReady,
		CreatedBy:                &f.user.ID,
	})
	require.NoError(t, err)
	require.NoError(t, tx.Commit())
	return <-done
}

// testSitePrefixLookupHook counts IP Block queries by SitePrefix ID.
type testSitePrefixLookupHook struct {
	reads int
}

func (hook *testSitePrefixLookupHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if (strings.Contains(event.Query, "site_prefix_id =") || strings.Contains(event.Query, "site_prefix_id IN")) &&
		strings.HasPrefix(event.Query, "SELECT") && !strings.Contains(strings.ToLower(event.Query), "count(") {
		hook.reads++
	}
	return ctx
}

func (*testSitePrefixLookupHook) AfterQuery(context.Context, *bun.QueryEvent) {}

// testLockAttemptHook cancels every Site fabric lock attempt after failAfter.
type testLockAttemptHook struct {
	failAfter int
	attempts  int
}

func (hook *testLockAttemptHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if !strings.Contains(event.Query, "pg_try_advisory_xact_lock") {
		return ctx
	}
	hook.attempts++
	if hook.failAfter > 0 && hook.attempts > hook.failAfter {
		cancelled, cancel := context.WithCancel(ctx)
		cancel()
		return cancelled
	}
	return ctx
}

func (*testLockAttemptHook) AfterQuery(context.Context, *bun.QueryEvent) {}

// testSiteDeletionHook deletes the Site when the second prefix locks it.
type testSiteDeletionHook struct {
	session *cdb.Session
	siteID  uuid.UUID
	reads   int
	err     error
}

func (hook *testSiteDeletionHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if strings.Contains(event.Query, "FROM \"site\" AS \"st\"") && strings.Contains(event.Query, "FOR NO KEY UPDATE") {
		hook.reads++
		if hook.reads == 2 {
			hook.err = cdbm.NewSiteDAO(hook.session).Delete(ctx, nil, hook.siteID)
		}
	}
	return ctx
}

func (*testSiteDeletionHook) AfterQuery(context.Context, *bun.QueryEvent) {}

// testStatusDetailFailureHook fails the remaining-th status detail insert.
type testStatusDetailFailureHook struct {
	remaining int
}

func (hook *testStatusDetailFailureHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
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

func (*testStatusDetailFailureHook) AfterQuery(context.Context, *bun.QueryEvent) {}

// testSiteLockHook pauses the first query that locks the Site row until resume
// is closed, without timing-dependent sleeps.
type testSiteLockHook struct {
	entered chan struct{}
	resume  chan struct{}
	once    sync.Once
}

func (hook *testSiteLockHook) BeforeQuery(ctx context.Context, _ *bun.QueryEvent) context.Context {
	return ctx
}

func (hook *testSiteLockHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
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

func TestManageSitePrefix_UpdateSitePrefixesInDB(t *testing.T) {
	idA, idB, idC := uuid.NewString(), uuid.NewString(), uuid.NewString()
	deleting := corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
	provisioning := corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_PROVISIONING
	errorState := corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_ERROR

	tests := []struct {
		name string
		// tenants are created in addition to the fixture's "tenant" org.
		tenants          []string
		existingIPBlocks []testExistingIPBlock
		// ipamPrefixes are DatacenterOnly IPAM entries without an IP Block.
		ipamPrefixes []string
		// previous is reconciled by an earlier inventory.
		previous []testSitePrefix
		reported []testSitePrefix
		// The flags below change the inventory built from reported.
		nilInventory               bool
		unspecifiedInventoryStatus bool
		failedInventory            bool
		useUnknownSiteID           bool
		inventoryAge               time.Duration
		finalPage                  bool
		omitPageMetadata           bool
		// failStatusDetailInsert fails the Nth status detail insert of the call under test.
		failStatusDetailInsert int
		wantErrIs              error
		wantErrContains        string
		wantIPBlocks           []testIPBlockState
		// wantIPAM and wantPublicIPAM list the CIDRs in the Site's DatacenterOnly
		// and Public IPAM namespaces.
		wantIPAM       []string
		wantPublicIPAM []string
		// wantLookups, when set, is the number of IP Block queries by SitePrefix ID.
		wantLookups  int
		wantLogs     []string
		wantNoErrors bool
		// wantAllocationBlocked lists linked roots that must reject new allocations.
		wantAllocationBlocked []string
		// retryIPBlocks and retryIPAM describe replay after a one-shot failure.
		retryIPBlocks []testIPBlockState
		retryIPAM     []string
	}{
		{
			name: "operator root name conflict gets a bounded recovered name",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr: "10.1.0.0/24",
					name: strings.Repeat("x", 256),
				},
			},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: strings.Repeat("x", 256),
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:      "10.1.0.0/24",
					name:      strings.Repeat("x", 256),
					status:    cdbm.IPBlockStatusReady,
					existing:  true,
					unchanged: true,
				},
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          strings.Repeat("x", 256-len("-recovered-")-8) + "-recovered-" + idA[:8],
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
			},
			wantIPAM: []string{"10.0.0.0/24", "10.1.0.0/24"},
		},
		{
			name: "restoration waits while another root uses the preserved REST name",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "reused-name",
					sitePrefixID: idA,
					retired:      true,
				},
				{
					cidr: "10.1.0.0/24",
					name: "reused-name",
				},
			},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:         "10.0.0.0/24",
					sitePrefixID: idA,
					name:         "reused-name",
					status:       cdbm.IPBlockStatusDeleting,
					deleted:      true,
					existing:     true,
					unchanged:    true,
				},
				{
					cidr:      "10.1.0.0/24",
					name:      "reused-name",
					status:    cdbm.IPBlockStatusReady,
					existing:  true,
					unchanged: true,
				},
			},
			wantIPAM:     []string{"10.1.0.0/24"},
			wantLogs:     []string{"preserved name", "already used by another root"},
			wantNoErrors: true,
		},
		{
			name: "occupied recovery name skips the prefix without creating IPAM",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr: "10.1.0.0/24",
					name: "root",
				},
				{
					cidr: "10.2.0.0/24",
					name: "root-recovered-" + idA[:8],
				},
			},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "root",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:      "10.1.0.0/24",
					name:      "root",
					status:    cdbm.IPBlockStatusReady,
					existing:  true,
					unchanged: true,
				},
				{
					cidr:      "10.2.0.0/24",
					name:      "root-recovered-" + idA[:8],
					status:    cdbm.IPBlockStatusReady,
					existing:  true,
					unchanged: true,
				},
			},
			wantIPAM:     []string{"10.1.0.0/24", "10.2.0.0/24"},
			wantLogs:     []string{"recovery name", "already used by another root"},
			wantNoErrors: true,
		},
		{
			name: "new operator Deleting report creates no root or IPAM and does not stop the page",
			reported: []testSitePrefix{
				{
					id:    idA,
					cidr:  "10.0.0.0/24",
					name:  "deleted-root",
					state: deleting,
				},
				{
					id:   idB,
					cidr: "10.1.0.0/24",
					name: "ready-root",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.1.0.0/24",
					sitePrefixID:  idB,
					name:          "ready-root",
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
			},
			wantIPAM: []string{"10.1.0.0/24"},
		},
		{
			name: "late final page creates its root without removing an earlier page",
			previous: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "first-root",
				},
			},
			reported: []testSitePrefix{
				{
					id:   idB,
					cidr: "10.1.0.0/24",
					name: "last-root",
				},
			},
			inventoryAge: 4 * time.Minute,
			finalPage:    true,
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "first-root",
					status:        cdbm.IPBlockStatusReady,
					existing:      true,
					unchanged:     true,
					statusDetails: 1,
				},
				{
					cidr:          "10.1.0.0/24",
					sitePrefixID:  idB,
					name:          "last-root",
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
			},
			wantIPAM: []string{"10.0.0.0/24", "10.1.0.0/24"},
		},
		{
			name: "optional page metadata does not gate public operator creation",
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "203.0.113.0/24",
					name: "public-root",
				},
			},
			omitPageMetadata: true,
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "203.0.113.0/24",
					sitePrefixID:  idA,
					name:          "public-root",
					public:        true,
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
			},
			wantPublicIPAM: []string{"203.0.113.0/24"},
		},
		{
			name:            "nil inventory is rejected",
			nilInventory:    true,
			wantErrContains: "nil inventory",
		},
		{
			name: "inventory without a status is rejected",
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			unspecifiedInventoryStatus: true,
			wantErrContains:            "unsupported SitePrefix inventory status",
		},
		{
			name: "failed inventory leaves IP Blocks unchanged",
			previous: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			reported: []testSitePrefix{
				{
					nilEntry: true,
				},
			},
			failedInventory: true,
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "core-name",
					status:        cdbm.IPBlockStatusReady,
					existing:      true,
					unchanged:     true,
					statusDetails: 1,
				},
			},
			wantIPAM: []string{"10.0.0.0/24"},
		},
		{
			name: "operator prefix creates a root and its IPAM entry",
			reported: []testSitePrefix{
				{
					id:          idA,
					cidr:        "10.0.0.0/24",
					name:        "core-name",
					description: "core description",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "core-name",
					description:   cutil.GetPtr("core description"),
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
			},
			wantIPAM:    []string{"10.0.0.0/24"},
			wantLookups: 2,
		},
		{
			name: "unnamed operator prefix is named after its CIDR",
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "10.0.0.0/24",
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
			},
			wantIPAM: []string{"10.0.0.0/24"},
		},
		{
			name: "Tenant prefix creates an unmanaged IP Block without IPAM",
			reported: []testSitePrefix{
				{
					id:            idA,
					cidr:          "10.0.0.0/24",
					name:          "tenant-prefix",
					state:         provisioning,
					tenantManaged: true,
					tenantOrg:     "tenant",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "tenant-prefix",
					status:        cdbm.IPBlockStatusProvisioning,
					tenantOrg:     "tenant",
					statusDetails: 1,
				},
			},
		},
		{
			name:    "Tenant prefixes with the same CIDR create an IP Block for each Tenant",
			tenants: []string{"other"},
			reported: []testSitePrefix{
				{
					id:            idA,
					cidr:          "10.0.0.0/24",
					name:          "tenant-prefix",
					tenantManaged: true,
					tenantOrg:     "tenant",
				},
				{
					id:            idB,
					cidr:          "10.0.0.0/24",
					name:          "other-prefix",
					tenantManaged: true,
					tenantOrg:     "other",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "tenant-prefix",
					status:        cdbm.IPBlockStatusReady,
					tenantOrg:     "tenant",
					statusDetails: 1,
				},
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idB,
					name:          "other-prefix",
					status:        cdbm.IPBlockStatusReady,
					tenantOrg:     "other",
					statusDetails: 1,
				},
			},
		},
		{
			name: "IP Blocks linked to a page are fetched together",
			previous: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
				{
					id:   idB,
					cidr: "10.1.0.0/24",
					name: "other-core-name",
				},
			},
			reported: []testSitePrefix{
				{
					id:    idA,
					cidr:  "10.0.0.0/24",
					name:  "core-name",
					state: deleting,
				},
				{
					id:    idB,
					cidr:  "10.1.0.0/24",
					name:  "other-core-name",
					state: deleting,
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "core-name",
					status:        cdbm.IPBlockStatusDeleting,
					existing:      true,
					statusDetails: 2,
				},
				{
					cidr:          "10.1.0.0/24",
					sitePrefixID:  idB,
					name:          "other-core-name",
					status:        cdbm.IPBlockStatusDeleting,
					existing:      true,
					statusDetails: 2,
				},
			},
			wantIPAM:    []string{"10.0.0.0/24", "10.1.0.0/24"},
			wantLookups: 1,
		},
		{
			name: "replaying an inventory changes nothing",
			previous: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "core-name",
					status:        cdbm.IPBlockStatusReady,
					existing:      true,
					unchanged:     true,
					statusDetails: 1,
				},
			},
			wantIPAM: []string{"10.0.0.0/24"},
		},
		{
			name: "linked root keeps its REST metadata and applies the reported lifecycle",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "existing-root",
					description:  "provider description",
					sitePrefixID: idA,
				},
			},
			reported: []testSitePrefix{
				{
					id:          idA,
					cidr:        "10.0.0.0/24",
					name:        "renamed",
					description: "changed",
					state:       deleting,
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "existing-root",
					description:   cutil.GetPtr("provider description"),
					status:        cdbm.IPBlockStatusDeleting,
					existing:      true,
					statusDetails: 1,
				},
			},
			wantIPAM: []string{"10.0.0.0/24"},
		},
		{
			name: "linked Tenant IP Block keeps its REST metadata and applies the reported lifecycle",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "tenant-prefix",
					description:  "tenant description",
					sitePrefixID: idA,
					tenantOrg:    "tenant",
				},
			},
			reported: []testSitePrefix{
				{
					id:            idA,
					cidr:          "10.0.0.0/24",
					name:          "renamed",
					state:         errorState,
					tenantManaged: true,
					tenantOrg:     "tenant",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "tenant-prefix",
					description:   cutil.GetPtr("tenant description"),
					status:        cdbm.IPBlockStatusError,
					tenantOrg:     "tenant",
					existing:      true,
					statusDetails: 1,
				},
			},
		},
		{
			name: "adopts an unlinked root with the same CIDR",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:           "10.0.0.0/24",
					name:           "existing-root",
					description:    "provider description",
					allocatedChild: true,
				},
			},
			reported: []testSitePrefix{
				{
					id:          idA,
					cidr:        "10.0.0.0/24",
					name:        "core-name",
					description: "core description",
					state:       deleting,
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "existing-root",
					description:   cutil.GetPtr("provider description"),
					status:        cdbm.IPBlockStatusDeleting,
					existing:      true,
					statusDetails: 1,
				},
			},
			wantIPAM: []string{"10.0.0.0/24", "10.0.0.0/28"},
		},
		{
			name: "does not adopt a root linked to another SitePrefix ID",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "existing-root",
					sitePrefixID: idB,
				},
			},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:         "10.0.0.0/24",
					sitePrefixID: idB,
					name:         "existing-root",
					status:       cdbm.IPBlockStatusReady,
					existing:     true,
					unchanged:    true,
				},
			},
			wantIPAM: []string{"10.0.0.0/24"},
		},
		{
			name: "does not adopt a root with another routing type",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:   "10.0.0.0/24",
					name:   "existing-root",
					public: true,
				},
			},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:      "10.0.0.0/24",
					name:      "existing-root",
					status:    cdbm.IPBlockStatusReady,
					public:    true,
					existing:  true,
					unchanged: true,
				},
			},
			wantPublicIPAM: []string{"10.0.0.0/24"},
		},
		{
			name: "skips a prefix that overlaps an existing root",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr: "10.0.0.0/16",
					name: "existing-root",
				},
			},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:      "10.0.0.0/16",
					name:      "existing-root",
					status:    cdbm.IPBlockStatusReady,
					existing:  true,
					unchanged: true,
				},
			},
			wantIPAM: []string{"10.0.0.0/16"},
		},
		{
			name: "resize preserves Deleting lifecycle with old root first",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "old-root",
					sitePrefixID: idA,
				},
			},
			reported: []testSitePrefix{
				{
					id:    idA,
					cidr:  "10.0.0.0/24",
					name:  "old-root",
					state: deleting,
				},
				{
					id:   idB,
					cidr: "10.0.0.0/25",
					name: "replacement-root",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "old-root",
					status:        cdbm.IPBlockStatusDeleting,
					existing:      true,
					statusDetails: 1,
				},
			},
			wantIPAM:              []string{"10.0.0.0/24"},
			wantLogs:              []string{"overlaps an existing root"},
			wantNoErrors:          true,
			wantAllocationBlocked: []string{idA},
		},
		{
			name: "resize preserves Deleting lifecycle with replacement first",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "old-root",
					sitePrefixID: idA,
				},
			},
			reported: []testSitePrefix{
				{
					id:   idB,
					cidr: "10.0.0.0/25",
					name: "replacement-root",
				},
				{
					id:    idA,
					cidr:  "10.0.0.0/24",
					name:  "old-root",
					state: deleting,
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "old-root",
					status:        cdbm.IPBlockStatusDeleting,
					existing:      true,
					statusDetails: 1,
				},
			},
			wantIPAM:              []string{"10.0.0.0/24"},
			wantLogs:              []string{"overlaps an existing root"},
			wantNoErrors:          true,
			wantAllocationBlocked: []string{idA},
		},
		{
			name: "skips a SitePrefix ID linked to another Site's IP Block",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "existing-root",
					sitePrefixID: idA,
					otherSite:    true,
				},
			},
			reported: []testSitePrefix{
				{
					id:    idA,
					cidr:  "10.0.0.0/24",
					name:  "core-name",
					state: deleting,
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:         "10.0.0.0/24",
					sitePrefixID: idA,
					name:         "existing-root",
					status:       cdbm.IPBlockStatusReady,
					otherSite:    true,
					existing:     true,
					unchanged:    true,
				},
			},
		},
		{
			name: "retired root reported as Ready is restored with its IPAM entry",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "2001:db8::/48",
					name:         "existing-root",
					description:  "provider description",
					public:       true,
					sitePrefixID: idA,
					retired:      true,
				},
			},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "2001:db8::/48",
					name: "core-name",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "2001:db8::/48",
					sitePrefixID:  idA,
					name:          "existing-root",
					description:   cutil.GetPtr("provider description"),
					status:        cdbm.IPBlockStatusReady,
					public:        true,
					existing:      true,
					statusDetails: 1,
				},
			},
			wantPublicIPAM: []string{"2001:db8::/48"},
		},
		{
			name: "retired root reported as Deleting stays retired without stopping the page",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "existing-root",
					sitePrefixID: idA,
					retired:      true,
				},
			},
			reported: []testSitePrefix{
				{
					id:    idA,
					cidr:  "10.0.0.0/24",
					name:  "core-name",
					state: deleting,
				},
				{
					id:   idB,
					cidr: "10.1.0.0/24",
					name: "other-core-name",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:         "10.0.0.0/24",
					sitePrefixID: idA,
					name:         "existing-root",
					status:       cdbm.IPBlockStatusDeleting,
					deleted:      true,
					existing:     true,
					unchanged:    true,
				},
				{
					cidr:          "10.1.0.0/24",
					sitePrefixID:  idB,
					name:          "other-core-name",
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
			},
			wantIPAM: []string{"10.1.0.0/24"},
		},
		{
			name: "retired root is not restored while another root has its CIDR",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "existing-root",
					sitePrefixID: idA,
					retired:      true,
				},
				{
					cidr:         "10.0.0.0/24",
					name:         "replacement-root",
					sitePrefixID: idB,
				},
			},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:         "10.0.0.0/24",
					sitePrefixID: idA,
					name:         "existing-root",
					status:       cdbm.IPBlockStatusDeleting,
					deleted:      true,
					existing:     true,
					unchanged:    true,
				},
				{
					cidr:         "10.0.0.0/24",
					sitePrefixID: idB,
					name:         "replacement-root",
					status:       cdbm.IPBlockStatusReady,
					existing:     true,
					unchanged:    true,
				},
			},
			wantIPAM: []string{"10.0.0.0/24"},
		},
		{
			name: "retired root reported with another CIDR stays retired",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "existing-root",
					sitePrefixID: idA,
					retired:      true,
				},
			},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/23",
					name: "core-name",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:         "10.0.0.0/24",
					sitePrefixID: idA,
					name:         "existing-root",
					status:       cdbm.IPBlockStatusDeleting,
					deleted:      true,
					existing:     true,
					unchanged:    true,
				},
			},
		},
		{
			name: "IPAM conflict leaves a retired root retired",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "existing-root",
					sitePrefixID: idA,
					retired:      true,
				},
			},
			ipamPrefixes: []string{"10.0.0.0/23"},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:         "10.0.0.0/24",
					sitePrefixID: idA,
					name:         "existing-root",
					status:       cdbm.IPBlockStatusDeleting,
					deleted:      true,
					existing:     true,
					unchanged:    true,
				},
			},
			wantIPAM: []string{"10.0.0.0/23"},
		},
		{
			name: "failed restoration rolls back the undelete and its IPAM entry",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "existing-root",
					sitePrefixID: idA,
					retired:      true,
				},
			},
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			failStatusDetailInsert: 1,
			wantIPBlocks: []testIPBlockState{
				{
					cidr:         "10.0.0.0/24",
					sitePrefixID: idA,
					name:         "existing-root",
					status:       cdbm.IPBlockStatusDeleting,
					deleted:      true,
					existing:     true,
					unchanged:    true,
				},
			},
		},
		{
			name: "recently deleted Tenant IP Block is not restored",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "tenant-prefix",
					sitePrefixID: idA,
					tenantOrg:    "tenant",
					retired:      true,
				},
			},
			reported: []testSitePrefix{
				{
					id:            idA,
					cidr:          "10.0.0.0/24",
					name:          "tenant-prefix",
					tenantManaged: true,
					tenantOrg:     "tenant",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:         "10.0.0.0/24",
					sitePrefixID: idA,
					name:         "tenant-prefix",
					status:       cdbm.IPBlockStatusDeleting,
					tenantOrg:    "tenant",
					deleted:      true,
					existing:     true,
					unchanged:    true,
				},
			},
		},
		{
			name: "Tenant IP Block deleted before the inventory interval is restored without IPAM",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "tenant-prefix",
					sitePrefixID: idA,
					tenantOrg:    "tenant",
					retired:      true,
					deletedAgo:   time.Hour,
				},
			},
			reported: []testSitePrefix{
				{
					id:            idA,
					cidr:          "10.0.0.0/24",
					name:          "core-name",
					tenantManaged: true,
					tenantOrg:     "tenant",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "tenant-prefix",
					status:        cdbm.IPBlockStatusReady,
					tenantOrg:     "tenant",
					existing:      true,
					statusDetails: 1,
				},
			},
		},
		{
			name: "unreconcilable entries do not hold back the rest of the page",
			reported: []testSitePrefix{
				{
					nilEntry: true,
				},
				{
					id:   idA,
					cidr: "10.1.0.1/24",
					name: "core-name",
				},
				{
					id:            idB,
					cidr:          "10.2.0.0/24",
					name:          "tenant-prefix",
					tenantManaged: true,
					tenantOrg:     "unknown",
				},
				{
					id:   idC,
					cidr: "10.3.0.0/24",
					name: "third-core-name",
				},
			},
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.3.0.0/24",
					sitePrefixID:  idC,
					name:          "third-core-name",
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
			},
			wantIPAM:     []string{"10.3.0.0/24"},
			wantLogs:     []string{idA, "network-aligned CIDR", idB, "unknown or ambiguous"},
			wantNoErrors: true,
		},
		{
			name: "failed prefix rolls back its IP Block and IPAM entry without stopping later entries",
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.1.0.0/24",
					name: "core-name",
				},
				{
					id:   idB,
					cidr: "10.2.0.0/24",
					name: "other-core-name",
				},
				{
					id:   idC,
					cidr: "10.3.0.0/24",
					name: "third-core-name",
				},
			},
			failStatusDetailInsert: 2,
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.1.0.0/24",
					sitePrefixID:  idA,
					name:          "core-name",
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
				{
					cidr:          "10.3.0.0/24",
					sitePrefixID:  idC,
					name:          "third-core-name",
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
			},
			wantIPAM: []string{"10.1.0.0/24", "10.3.0.0/24"},
			retryIPBlocks: []testIPBlockState{
				{
					cidr:          "10.1.0.0/24",
					sitePrefixID:  idA,
					name:          "core-name",
					status:        cdbm.IPBlockStatusReady,
					existing:      true,
					unchanged:     true,
					statusDetails: 1,
				},
				{
					cidr:          "10.2.0.0/24",
					sitePrefixID:  idB,
					name:          "other-core-name",
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
				{
					cidr:          "10.3.0.0/24",
					sitePrefixID:  idC,
					name:          "third-core-name",
					status:        cdbm.IPBlockStatusReady,
					existing:      true,
					unchanged:     true,
					statusDetails: 1,
				},
			},
			retryIPAM: []string{"10.1.0.0/24", "10.2.0.0/24", "10.3.0.0/24"},
		},
		{
			name: "unknown Site returns an error",
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			useUnknownSiteID: true,
			wantErrIs:        cdb.ErrDoesNotExist,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			f := newTestFixture(t)
			var output bytes.Buffer
			previousLogger := log.Logger
			log.Logger = zerolog.New(&output)
			t.Cleanup(func() { log.Logger = previousLogger })

			f.addTenants(t, tt.tenants)
			f.createExistingIPBlocks(t, tt.existingIPBlocks)
			storage := ipam.NewIpamStorage(f.session.DB, nil)
			for _, cidr := range tt.ipamPrefixes {
				prefix := netip.MustParsePrefix(cidr)
				_, err := ipam.CreateIpamEntryForIPBlock(ctx, storage, prefix.Addr().String(), prefix.Bits(),
					cdbm.IPBlockRoutingTypeDatacenterOnly, f.site.InfrastructureProviderID.String(), f.site.ID.String())
				require.NoError(t, err)
			}
			collectionTime := timestamppb.New(time.Now().Add(-tt.inventoryAge))
			if tt.previous != nil {
				previous := testSitePrefixInventory(tt.previous)
				previous.Timestamp = collectionTime
				require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, previous))
			}
			before := f.blocksByID(t)
			site := f.currentSite(t)

			inventory := testSitePrefixInventory(tt.reported)
			inventory.Timestamp = collectionTime
			if tt.finalPage {
				inventory.InventoryPage.CurrentPage = 2
				inventory.InventoryPage.TotalPages = 2
				inventory.InventoryPage.TotalItems += int32(len(tt.previous))
				for _, prefix := range tt.previous {
					inventory.InventoryPage.ItemIds = append(inventory.InventoryPage.ItemIds, prefix.id)
				}
			}
			if tt.omitPageMetadata {
				inventory.Timestamp = nil
				inventory.InventoryPage = nil
			}
			if tt.unspecifiedInventoryStatus {
				inventory.InventoryStatus = corev1.InventoryStatus_INVENTORY_STATUS_UNSPECIFIED
			}
			if tt.failedInventory {
				inventory.InventoryStatus = corev1.InventoryStatus_INVENTORY_STATUS_FAILED
			}
			if tt.nilInventory {
				inventory = nil
			}
			siteID := f.site.ID
			if tt.useUnknownSiteID {
				siteID = uuid.New()
			}
			if tt.failStatusDetailInsert > 0 {
				f.session.DB.AddQueryHook(&testStatusDetailFailureHook{remaining: tt.failStatusDetailInsert})
			}
			lookups := &testSitePrefixLookupHook{}
			f.session.DB.AddQueryHook(lookups)

			err := f.manager.UpdateSitePrefixesInDB(ctx, siteID, inventory)
			switch {
			case tt.wantErrIs != nil:
				require.ErrorIs(t, err, tt.wantErrIs)
			case tt.wantErrContains != "":
				require.ErrorContains(t, err, tt.wantErrContains)
			default:
				require.NoError(t, err)
			}
			if tt.wantLookups > 0 {
				require.Equal(t, tt.wantLookups, lookups.reads)
			}
			require.ElementsMatch(t, tt.wantIPBlocks, f.ipBlockStates(t, before))
			require.ElementsMatch(t, tt.wantIPAM, f.ipamCIDRs(t, cdbm.IPBlockRoutingTypeDatacenterOnly))
			require.ElementsMatch(t, tt.wantPublicIPAM, f.ipamCIDRs(t, cdbm.IPBlockRoutingTypePublic))
			require.Equal(t, site, f.currentSite(t))
			for _, want := range tt.wantLogs {
				require.Contains(t, output.String(), want)
			}
			if tt.wantNoErrors {
				require.NotContains(t, output.String(), `"level":"error"`)
			}
			for _, id := range tt.wantAllocationBlocked {
				root := f.linkedBlock(t, id)
				tx, err := cdb.BeginTx(ctx, f.session, nil)
				require.NoError(t, err)
				err = ipam.LockAndValidateParentIPBlockForAllocation(ctx, tx, f.session, root)
				require.NoError(t, tx.Rollback())
				require.ErrorContains(t, err, "not Ready")
			}
			if tt.retryIPBlocks != nil {
				beforeRetry := f.blocksByID(t)
				require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, siteID, inventory))
				require.ElementsMatch(t, tt.retryIPBlocks, f.ipBlockStates(t, beforeRetry))
				require.ElementsMatch(t, tt.retryIPAM, f.ipamCIDRs(t, cdbm.IPBlockRoutingTypeDatacenterOnly))
			}
		})
	}

	// Another writer holds the Site fabric lock when the call under test starts.
	lockReported := []testSitePrefix{
		{
			id:   idA,
			cidr: "10.0.0.0/24",
			name: "core-name",
		},
	}
	lockTests := []struct {
		name string
		// failAfter cancels every lock attempt after that one.
		failAfter int
		// wantLogLevel is the one of warn and error that is logged.
		wantLogLevel string
	}{
		{
			name:         "exhausted lock contention skips the prefix with a warning",
			wantLogLevel: "warn",
		},
		{
			name:         "database failure after lock contention logs an error",
			failAfter:    1,
			wantLogLevel: "error",
		},
	}

	for _, tt := range lockTests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			f := newTestFixture(t)
			var output bytes.Buffer
			previousLogger := log.Logger
			log.Logger = zerolog.New(&output)
			t.Cleanup(func() { log.Logger = previousLogger })

			tx, err := cdb.BeginTx(ctx, f.session, nil)
			require.NoError(t, err)
			defer func() { _ = tx.Rollback() }()
			require.NoError(t, tx.AcquireAdvisoryLock(ctx, cdbm.SiteFabricIPBlockLockID(f.site.InfrastructureProviderID, f.site.ID), false))
			f.session.DB.AddQueryHook(&testLockAttemptHook{failAfter: tt.failAfter})

			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testSitePrefixInventory(lockReported)))
			require.Empty(t, f.ipBlockStates(t, nil))
			for _, level := range []string{"warn", "error"} {
				require.Equal(t, level == tt.wantLogLevel, strings.Contains(output.String(), `"level":"`+level+`"`), level)
			}
		})
	}

	// A Site deletion or an Allocation races the call under test.
	concurrentTests := []struct {
		name             string
		existingIPBlocks []testExistingIPBlock
		reported         []testSitePrefix
		writer           testConcurrentWriter
		wantIPBlocks     []testIPBlockState
		wantIPAM         []string
		wantSiteDeleted  bool
	}{
		{
			name: "Site deletion that commits while the inventory waits stops reconciliation",
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			writer:          testSiteDeletionFirst,
			wantSiteDeleted: true,
		},
		{
			name: "Site deletion waits for the prefix the inventory is reconciling",
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
			},
			writer: testSiteDeletionWaiting,
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "core-name",
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
			},
			wantIPAM:        []string{"10.0.0.0/24"},
			wantSiteDeleted: true,
		},
		{
			name: "Site deletion between prefixes stops later prefixes",
			reported: []testSitePrefix{
				{
					id:   idA,
					cidr: "10.0.0.0/24",
					name: "core-name",
				},
				{
					id:   idB,
					cidr: "10.1.0.0/24",
					name: "other-core-name",
				},
			},
			writer: testSiteDeletionBetweenPrefixes,
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "core-name",
					status:        cdbm.IPBlockStatusReady,
					statusDetails: 1,
				},
			},
			wantIPAM:        []string{"10.0.0.0/24"},
			wantSiteDeleted: true,
		},
		{
			name: "Allocation can finish while the inventory holds the Site and waits for the root",
			existingIPBlocks: []testExistingIPBlock{
				{
					cidr:         "10.0.0.0/24",
					name:         "existing-root",
					sitePrefixID: idA,
				},
			},
			reported: []testSitePrefix{
				{
					id:    idA,
					cidr:  "10.0.0.0/24",
					name:  "core-name",
					state: deleting,
				},
			},
			writer: testAllocationWaiting,
			wantIPBlocks: []testIPBlockState{
				{
					cidr:          "10.0.0.0/24",
					sitePrefixID:  idA,
					name:          "existing-root",
					status:        cdbm.IPBlockStatusDeleting,
					existing:      true,
					statusDetails: 1,
				},
				{
					cidr:      "10.0.0.0/25",
					name:      "allocated-child",
					status:    cdbm.IPBlockStatusReady,
					tenantOrg: "tenant",
				},
			},
			wantIPAM: []string{"10.0.0.0/24"},
		},
	}

	for _, tt := range concurrentTests {
		t.Run(tt.name, func(t *testing.T) {
			f := newTestFixture(t)
			f.createExistingIPBlocks(t, tt.existingIPBlocks)
			before := f.blocksByID(t)
			inventory := testSitePrefixInventory(tt.reported)

			var err error
			switch tt.writer {
			case testSiteDeletionFirst:
				err = f.runWithSiteDeletionFirst(t, inventory)
			case testSiteDeletionWaiting:
				err = f.runWithSiteDeletionWaiting(t, inventory)
			case testSiteDeletionBetweenPrefixes:
				err = f.runWithSiteDeletionBetweenPrefixes(t, inventory)
			case testAllocationWaiting:
				err = f.runWithAllocationWaiting(t, inventory)
			}

			require.NoError(t, err)
			require.ElementsMatch(t, tt.wantIPBlocks, f.ipBlockStates(t, before))
			require.ElementsMatch(t, tt.wantIPAM, f.ipamCIDRs(t, cdbm.IPBlockRoutingTypeDatacenterOnly))
			require.Equal(t, tt.wantSiteDeleted, f.currentSite(t).Deleted != nil)
		})
	}
}

func TestValidatePrefix(t *testing.T) {
	id := uuid.NewString()
	provisioning := corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_PROVISIONING

	tests := []struct {
		name    string
		prefix  testSitePrefix
		wantErr bool
	}{
		{
			name: "operator IPv4 prefix is valid",
			prefix: testSitePrefix{
				id:   id,
				cidr: "10.0.0.0/24",
				name: "core-name",
			},
		},
		{
			name: "nil SitePrefix is rejected",
			prefix: testSitePrefix{
				nilEntry: true,
			},
			wantErr: true,
		},
		{
			name: "missing config is rejected",
			prefix: testSitePrefix{
				id:         id,
				cidr:       "10.0.0.0/24",
				name:       "core-name",
				omitConfig: true,
			},
			wantErr: true,
		},
		{
			name: "missing status is rejected",
			prefix: testSitePrefix{
				id:         id,
				cidr:       "10.0.0.0/24",
				name:       "core-name",
				omitStatus: true,
			},
			wantErr: true,
		},
		{
			name: "missing metadata is rejected",
			prefix: testSitePrefix{
				id:           id,
				cidr:         "10.0.0.0/24",
				omitMetadata: true,
			},
			wantErr: true,
		},
		{
			name: "invalid ID is rejected",
			prefix: testSitePrefix{
				id:   "invalid",
				cidr: "10.0.0.0/24",
				name: "core-name",
			},
			wantErr: true,
		},
		{
			name: "nil UUID is rejected",
			prefix: testSitePrefix{
				id:   uuid.Nil.String(),
				cidr: "10.0.0.0/24",
				name: "core-name",
			},
			wantErr: true,
		},
		{
			name: "noncanonical ID is rejected",
			prefix: testSitePrefix{
				id:   strings.ToUpper(id),
				cidr: "10.0.0.0/24",
				name: "core-name",
			},
			wantErr: true,
		},
		{
			name: "CIDR with host bits is rejected",
			prefix: testSitePrefix{
				id:   id,
				cidr: "10.0.0.1/24",
				name: "core-name",
			},
			wantErr: true,
		},
		{
			name: "unspecified routing scope is rejected",
			prefix: testSitePrefix{
				id:                      id,
				cidr:                    "10.0.0.0/24",
				name:                    "core-name",
				unspecifiedRoutingScope: true,
			},
			wantErr: true,
		},
		{
			name: "unspecified authority is rejected",
			prefix: testSitePrefix{
				id:                   id,
				cidr:                 "10.0.0.0/24",
				name:                 "core-name",
				unspecifiedAuthority: true,
			},
			wantErr: true,
		},
		{
			name: "operator prefix with a Tenant org is rejected",
			prefix: testSitePrefix{
				id:        id,
				cidr:      "10.0.0.0/24",
				name:      "core-name",
				tenantOrg: "tenant",
			},
			wantErr: true,
		},
		{
			name: "operator prefix in Provisioning is rejected",
			prefix: testSitePrefix{
				id:    id,
				cidr:  "10.0.0.0/24",
				name:  "core-name",
				state: provisioning,
			},
			wantErr: true,
		},
		{
			name: "Tenant prefix without a lifecycle state is rejected",
			prefix: testSitePrefix{
				id:               id,
				cidr:             "10.0.0.0/24",
				name:             "tenant-prefix",
				tenantManaged:    true,
				tenantOrg:        "tenant",
				unspecifiedState: true,
			},
			wantErr: true,
		},
		{
			name: "Tenant /8 prefix is valid",
			prefix: testSitePrefix{
				id:            id,
				cidr:          "10.0.0.0/8",
				name:          "tenant-prefix",
				tenantManaged: true,
				tenantOrg:     "tenant",
			},
		},
		{
			name: "Tenant /31 prefix is valid",
			prefix: testSitePrefix{
				id:            id,
				cidr:          "192.168.0.0/31",
				name:          "tenant-prefix",
				tenantManaged: true,
				tenantOrg:     "tenant",
			},
		},
		{
			name: "Tenant prefix shorter than /8 is rejected",
			prefix: testSitePrefix{
				id:            id,
				cidr:          "10.0.0.0/7",
				name:          "tenant-prefix",
				tenantManaged: true,
				tenantOrg:     "tenant",
			},
			wantErr: true,
		},
		{
			name: "Tenant /32 prefix is rejected",
			prefix: testSitePrefix{
				id:            id,
				cidr:          "10.0.0.0/32",
				name:          "tenant-prefix",
				tenantManaged: true,
				tenantOrg:     "tenant",
			},
			wantErr: true,
		},
		{
			name: "Tenant public prefix is rejected",
			prefix: testSitePrefix{
				id:            id,
				cidr:          "203.0.113.0/24",
				name:          "tenant-prefix",
				tenantManaged: true,
				tenantOrg:     "tenant",
			},
			wantErr: true,
		},
		{
			name: "Tenant IPv6 prefix is rejected",
			prefix: testSitePrefix{
				id:            id,
				cidr:          "fd00::/64",
				name:          "tenant-prefix",
				tenantManaged: true,
				tenantOrg:     "tenant",
			},
			wantErr: true,
		},
		{
			name: "name longer than 256 characters is rejected",
			prefix: testSitePrefix{
				id:   id,
				cidr: "10.0.0.0/24",
				name: strings.Repeat("x", 257),
			},
			wantErr: true,
		},
		{
			name: "non-ASCII name is rejected",
			prefix: testSitePrefix{
				id:   id,
				cidr: "10.0.0.0/24",
				name: "prefix\u00e9",
			},
			wantErr: true,
		},
		{
			name: "control character in name is rejected",
			prefix: testSitePrefix{
				id:   id,
				cidr: "10.0.0.0/24",
				name: "prefix\x00name",
			},
			wantErr: true,
		},
		{
			name: "NUL in description is rejected",
			prefix: testSitePrefix{
				id:          id,
				cidr:        "10.0.0.0/24",
				name:        "core-name",
				description: "description\x00",
			},
			wantErr: true,
		},
		{
			name: "non-ASCII label key is rejected",
			prefix: testSitePrefix{
				id:     id,
				cidr:   "10.0.0.0/24",
				name:   "core-name",
				labels: []*corev1.Label{{Key: "key\u00e9"}},
			},
			wantErr: true,
		},
		{
			name: "duplicate label keys are rejected",
			prefix: testSitePrefix{
				id:   id,
				cidr: "10.0.0.0/24",
				name: "core-name",
				labels: []*corev1.Label{
					{Key: "x"},
					{Key: "x"},
				},
			},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := validatePrefix(tt.prefix.toProto())
			if !tt.wantErr {
				require.NoError(t, err)
				return
			}
			var applicationErr *temporal.ApplicationError
			require.ErrorAs(t, err, &applicationErr)
			require.True(t, applicationErr.NonRetryable())
		})
	}
}
