// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dhcpoptionset

import (
	"context"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
)

type dhcpOptionSetFixture struct {
	tenant *cdbm.Tenant
	site   *cdbm.Site
}

func testBuildFixture(t *testing.T, dbSession *cdb.Session, siteName string) dhcpOptionSetFixture {
	ipOrg := "test-provider-org-" + siteName
	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{"FORGE_PROVIDER_ADMIN"})
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-provider-"+siteName, ipOrg, ipu)

	tnOrg := "test-tenant-org-" + siteName
	tnu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg}, []string{"FORGE_TENANT_ADMIN"})
	tn := util.TestBuildTenant(t, dbSession, "Test Tenant "+siteName, tnOrg, nil, tnu)

	st := util.TestBuildSite(t, dbSession, ip, siteName, cdbm.SiteStatusRegistered, nil, ipu)
	return dhcpOptionSetFixture{tenant: tn, site: st}
}

// testBuildDhcpOptionSet creates a stored set aged past the stale inventory threshold
func testBuildDhcpOptionSet(t *testing.T, dbSession *cdb.Session, fx dhcpOptionSetFixture, name string, status cdbm.DhcpOptionSetStatus, missing bool) *cdbm.DhcpOptionSet {
	ctx := context.Background()
	dosDAO := cdbm.NewDhcpOptionSetDAO(dbSession)
	dos, err := dosDAO.Create(ctx, nil, cdbm.DhcpOptionSetCreateInput{
		Name:      name,
		TenantOrg: fx.tenant.Org,
		SiteID:    fx.site.ID,
		TenantID:  fx.tenant.ID,
		Options:   &cdbm.DhcpOptions{DhcpOptions: &corev1.DhcpOptions{V4: &corev1.DhcpOptionsV4{DnsServers: []string{"10.0.0.53"}}}},
		Status:    status,
		Version:   cutil.GetPtr("V1-T1"),
		CreatedBy: fx.tenant.CreatedBy,
	})
	require.NoError(t, err)
	if missing {
		_, err = dosDAO.Update(ctx, nil, cdbm.DhcpOptionSetUpdateInput{DhcpOptionSetID: dos.ID, IsMissingOnSite: cutil.GetPtr(true)})
		require.NoError(t, err)
	}
	past := time.Now().Add(-2 * cutil.DefaultInventoryReceiptInterval)
	_, err = dbSession.DB.Exec("UPDATE dhcp_option_set SET created = ?, updated = ? WHERE id = ?", past, past, dos.ID.String())
	require.NoError(t, err)
	return dos
}

func coreDhcpOptionSet(id uuid.UUID, org, name, version, dnsServer string) *corev1.DhcpOptionSet {
	return &corev1.DhcpOptionSet{
		Id:                   &corev1.DhcpOptionSetId{Value: id.String()},
		TenantOrganizationId: org,
		Metadata:             &corev1.Metadata{Name: name},
		Options:              &corev1.DhcpOptions{V4: &corev1.DhcpOptionsV4{DnsServers: []string{dnsServer}}},
		Version:              version,
	}
}

func singlePageInventory(sets ...*corev1.DhcpOptionSet) *corev1.DhcpOptionSetInventory {
	ids := []string{}
	for _, set := range sets {
		ids = append(ids, set.GetId().GetValue())
	}
	return &corev1.DhcpOptionSetInventory{
		DhcpOptionSets:  sets,
		Timestamp:       timestamppb.Now(),
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		InventoryPage:   &corev1.InventoryPage{TotalPages: 1, CurrentPage: 1, PageSize: 25, TotalItems: int32(len(sets)), ItemIds: ids},
	}
}

func TestManageDhcpOptionSet_UpdateDhcpOptionSetsInDB(t *testing.T) {
	ctx := context.Background()
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()
	util.TestSetupSchema(t, dbSession)

	dosDAO := cdbm.NewDhcpOptionSetDAO(dbSession)
	manager := NewManageDhcpOptionSet(dbSession)

	tests := []struct {
		name string
		// run stores the starting state, applies one inventory, and checks the persisted row
		run func(t *testing.T, fx dhcpOptionSetFixture)
	}{
		{
			name: "a reported set with a new Site version takes the Site values",
			run: func(t *testing.T, fx dhcpOptionSetFixture) {
				dos := testBuildDhcpOptionSet(t, dbSession, fx, "pxe-boot", cdbm.DhcpOptionSetStatusReady, false)
				require.NoError(t, manager.UpdateDhcpOptionSetsInDB(ctx, fx.site.ID,
					singlePageInventory(coreDhcpOptionSet(dos.ID, fx.tenant.Org, "pxe-boot-v2", "V2-T2", "10.0.0.99"))))

				got, err := dosDAO.GetByID(ctx, nil, dos.ID, nil)
				require.NoError(t, err)
				assert.Equal(t, "pxe-boot-v2", got.Name)
				assert.Equal(t, "V2-T2", got.Version)
				assert.Equal(t, []string{"10.0.0.99"}, got.Options.GetV4().GetDnsServers())
			},
		},
		{
			name: "a set only the Site reports is created with its Site ID",
			run: func(t *testing.T, fx dhcpOptionSetFixture) {
				id := uuid.New()
				require.NoError(t, manager.UpdateDhcpOptionSetsInDB(ctx, fx.site.ID,
					singlePageInventory(coreDhcpOptionSet(id, fx.tenant.Org, "recovered", "V1-T1", "10.0.0.53"))))

				got, err := dosDAO.GetByID(ctx, nil, id, nil)
				require.NoError(t, err)
				assert.Equal(t, fx.tenant.ID, got.TenantID)
				assert.Equal(t, cdbm.DhcpOptionSetStatusReady, got.Status)
				assert.Equal(t, "V1-T1", got.Version)
			},
		},
		{
			name: "a set the Site no longer reports is flagged missing",
			run: func(t *testing.T, fx dhcpOptionSetFixture) {
				dos := testBuildDhcpOptionSet(t, dbSession, fx, "gone", cdbm.DhcpOptionSetStatusReady, false)
				require.NoError(t, manager.UpdateDhcpOptionSetsInDB(ctx, fx.site.ID, singlePageInventory()))

				got, err := dosDAO.GetByID(ctx, nil, dos.ID, nil)
				require.NoError(t, err)
				assert.True(t, got.IsMissingOnSite)
				assert.Equal(t, cdbm.DhcpOptionSetStatusError, got.Status)
			},
		},
		{
			name: "a missing set the Site reports again is Ready",
			run: func(t *testing.T, fx dhcpOptionSetFixture) {
				dos := testBuildDhcpOptionSet(t, dbSession, fx, "back", cdbm.DhcpOptionSetStatusError, true)
				require.NoError(t, manager.UpdateDhcpOptionSetsInDB(ctx, fx.site.ID,
					singlePageInventory(coreDhcpOptionSet(dos.ID, fx.tenant.Org, "back", "V1-T1", "10.0.0.53"))))

				got, err := dosDAO.GetByID(ctx, nil, dos.ID, nil)
				require.NoError(t, err)
				assert.False(t, got.IsMissingOnSite)
				assert.Equal(t, cdbm.DhcpOptionSetStatusReady, got.Status)
			},
		},
	}
	for i, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tt.run(t, testBuildFixture(t, dbSession, "test-site-"+string(rune('a'+i))))
		})
	}
}
