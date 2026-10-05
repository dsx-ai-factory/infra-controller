// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestIPBlockManagedUpMigration(t *testing.T) {
	ctx := context.Background()
	for _, schema := range []string{"predecessor", "fresh model"} {
		t.Run(schema, func(t *testing.T) {
			session := util.GetTestDBSession(t, true)
			defer session.Close()
			model.TestSetupSchema(t, session)
			user := model.TestBuildUser(t, session, uuid.NewString(), "ip-block-migration", []string{"FORGE_PROVIDER_ADMIN"})
			provider := model.TestBuildInfrastructureProvider(t, session, "provider", "ip-block-migration", user)
			site := model.TestBuildSite(t, session, provider, "site", user)
			tenant := model.TestBuildTenant(t, session, "tenant", "tenant", user)
			if schema == "predecessor" {
				_, err := session.DB.ExecContext(ctx, "ALTER TABLE ip_block DROP COLUMN managed")
				require.NoError(t, err)
			}
			var blocks []model.IPBlock
			for _, tc := range []struct {
				name         string
				tenantID     *uuid.UUID
				sitePrefixID *uuid.UUID
				deleted      bool
			}{
				{name: "legacy provider root"},
				{name: "linked provider root", sitePrefixID: cutil.GetPtr(uuid.New())},
				{name: "allocated tenant block", tenantID: &tenant.ID},
				{name: "tenant-created block", tenantID: &tenant.ID, sitePrefixID: cutil.GetPtr(uuid.New())},
				{name: "retired tenant-created block", tenantID: &tenant.ID, sitePrefixID: cutil.GetPtr(uuid.New()), deleted: true},
			} {
				block := model.IPBlock{ID: uuid.New(), Name: tc.name,
					SiteID: site.ID, InfrastructureProviderID: provider.ID,
					TenantID: tc.tenantID, SitePrefixID: tc.sitePrefixID,
					RoutingType: model.IPBlockRoutingTypeDatacenterOnly, Prefix: "10.0.0.0", PrefixLength: 24,
					ProtocolVersion: model.IPBlockProtocolVersionV4, Status: model.IPBlockStatusReady,
					CreatedBy: &user.ID, Managed: true,
				}
				if tc.deleted {
					block.Deleted = cutil.GetPtr(site.Created)
				}
				query := session.DB.NewInsert().Model(&block)
				if schema == "predecessor" {
					query.ExcludeColumn("managed")
				}
				_, err := query.Exec(ctx)
				require.NoError(t, err)
				err = session.DB.NewSelect().Model(&block).ExcludeColumn("managed").WhereAllWithDeleted().WherePK().Scan(ctx)
				require.NoError(t, err)
				blocks = append(blocks, block)
			}
			require.NoError(t, ipBlockManagedUpMigration(ctx, session.DB))
			for _, block := range blocks {
				t.Run(block.Name, func(t *testing.T) {
					var stored model.IPBlock
					err := session.DB.NewSelect().Model(&stored).WhereAllWithDeleted().Where("ipb.id = ?", block.ID).Scan(ctx)
					require.NoError(t, err)
					block.Managed = block.TenantID == nil || block.SitePrefixID == nil
					require.Equal(t, block, stored)
				})
			}
			// An old provider/Allocation writer omits the new column.
			legacy := blocks[2]
			legacy.ID = uuid.New()
			_, err := session.DB.NewInsert().Model(&legacy).ExcludeColumn("managed").Exec(ctx)
			require.NoError(t, err)
			stored, err := model.NewIPBlockDAO(session).GetByID(ctx, nil, legacy.ID, nil)
			require.NoError(t, err)
			require.True(t, stored.Managed)
		})
	}
}
