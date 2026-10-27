// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"context"
	"net/netip"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func (f testFixture) allocateRoot(t *testing.T, root *cdbm.IPBlock, length int) *cdbm.IPBlock {
	t.Helper()
	ctx := context.Background()
	allocation := util.TestBuildAllocation(t, f.session, &cdbm.InfrastructureProvider{ID: f.site.InfrastructureProviderID}, f.tenants["tenant"], f.site, "root allocation")
	var child *cdbm.IPBlock
	err := cdb.WithTx(ctx, f.session, func(tx *cdb.Tx) error {
		prefix, err := ipam.CreateChildIpamEntryForIPBlock(ctx, tx, f.session, ipam.NewIpamStorage(f.session.DB, tx.GetBunTx()), root, length)
		if err != nil {
			return err
		}
		cidr := netip.MustParsePrefix(prefix.Cidr)
		child, err = cdbm.NewIPBlockDAO(f.session).Create(ctx, tx, cdbm.IPBlockCreateInput{
			Name: "allocated-child", SiteID: f.site.ID, InfrastructureProviderID: f.site.InfrastructureProviderID,
			TenantID: &f.tenants["tenant"].ID, RoutingType: root.RoutingType,
			Prefix: cidr.Addr().String(), PrefixLength: cidr.Bits(), ProtocolVersion: root.ProtocolVersion,
			Status: cdbm.IPBlockStatusReady, CreatedBy: &f.user.ID,
		})
		if err != nil {
			return err
		}
		_, err = cdbm.NewAllocationConstraintDAO(f.session).Create(ctx, tx, cdbm.AllocationConstraintCreateInput{
			AllocationID: allocation.ID, ResourceType: cdbm.AllocationResourceTypeIPBlock, ResourceTypeID: root.ID,
			DerivedResourceID: &child.ID, ConstraintType: cdbm.AllocationConstraintTypeOnDemand, ConstraintValue: length, CreatedBy: f.user.ID,
		})
		return err
	})
	require.NoError(t, err)
	return child
}

func checkSitePrefixAssociation(t *testing.T) {
	t.Helper()
	for _, tc := range []struct {
		name           string
		absence, adopt bool
	}{
		{name: "reported Ready after unlink"},
		{name: "reported Deleting after adoption", adopt: true},
		{name: "absence candidate after adoption", absence: true, adopt: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			f := newTestFixture(t)
			root := f.root(t, true)
			ageBlock(t, f, root)
			child := f.allocateRoot(t, root, 28)
			old := testPrefix()
			old.Id.Value = root.SitePrefixID.String()
			if tc.adopt {
				old.Status.LifecycleState = corev1.SitePrefixLifecycleState_SITE_PREFIX_LIFECYCLE_STATE_DELETING
			}
			inventory := testInventory(old)
			// Isolate reported-entry handling from the final-page absence sweep.
			inventory.InventoryPage = &corev1.InventoryPage{CurrentPage: 1, TotalPages: 2, TotalItems: 2}
			if tc.absence {
				inventory = testSitePrefixInventory(nil)
			}
			replacement := testPrefix()
			var before *cdbm.IPBlock
			var detailsBefore []cdbm.StatusDetail
			f.session.DB.AddQueryHook(&testLockAttemptHook{beforeFirstAttempt: func() {
				require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testSitePrefixInventory(nil)))
				if tc.adopt {
					require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, testInventory(replacement)))
				}
				var err error
				before, err = cdbm.NewIPBlockDAO(f.session).GetByID(ctx, nil, root.ID, nil)
				require.NoError(t, err)
				detailsBefore, _, err = cdbm.NewStatusDetailDAO(f.session).GetAll(ctx, nil,
					cdbm.StatusDetailFilterInput{EntityIDs: []string{root.ID.String()}}, paginator.PageInput{})
				require.NoError(t, err)
			}})
			require.NoError(t, f.manager.UpdateSitePrefixesInDB(ctx, f.site.ID, inventory))
			after, err := cdbm.NewIPBlockDAO(f.session).GetByID(ctx, nil, root.ID, nil)
			require.NoError(t, err)
			require.Equal(t, before, after, "stale inventory must preserve the current association and lifecycle")
			wantStatus := cdbm.IPBlockStatusDeleting
			var wantLink *uuid.UUID
			if tc.adopt {
				wantStatus = cdbm.IPBlockStatusReady
				wantLink = cutil.GetPtr(uuid.MustParse(replacement.Id.Value))
			}
			require.Equal(t, wantStatus, after.Status)
			require.Equal(t, wantLink, after.SitePrefixID)
			details, _, err := cdbm.NewStatusDetailDAO(f.session).GetAll(ctx, nil,
				cdbm.StatusDetailFilterInput{EntityIDs: []string{root.ID.String()}}, paginator.PageInput{})
			require.NoError(t, err)
			require.Equal(t, detailsBefore, details)
			gateErr := cdb.WithTx(ctx, f.session, func(tx *cdb.Tx) error {
				return ipam.LockAndValidateParentIPBlockForAllocation(ctx, tx, f.session, after)
			})
			if tc.adopt {
				require.NoError(t, gateErr)
			} else {
				require.ErrorContains(t, gateErr, "not Ready")
			}
			preserved, err := cdbm.NewIPBlockDAO(f.session).GetByID(ctx, nil, child.ID, nil)
			require.NoError(t, err)
			require.Equal(t, child, preserved)
			_, err = ipam.NewIpamStorage(f.session.DB, nil).ReadPrefix(ctx, ipam.GetCidrForIPBlock(ctx, child.Prefix, child.PrefixLength), f.namespace())
			require.NoError(t, err)
		})
	}
}
