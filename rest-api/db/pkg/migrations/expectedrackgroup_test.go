// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/migrate"
)

func TestExpectedRackGroupMigration(t *testing.T) {
	ctx := context.Background()
	session := util.GetTestDBSession(t, true)
	defer session.Close()
	model.TestSetupSchema(t, session)
	user := model.TestBuildUser(t, session, uuid.NewString(), "rack-group-migration", []string{"FORGE_PROVIDER_ADMIN"})
	provider := model.TestBuildInfrastructureProvider(t, session, "provider", "rack-group-migration", user)
	site := model.TestBuildSite(t, session, provider, "site", user)
	otherSite := model.TestBuildSite(t, session, provider, "other-site", user)
	target := migrate.NewMigrations()
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20260917160100" {
			target.Add(migration)
		}
	}
	require.Len(t, target.Sorted(), 1)
	migrator := migrate.NewMigrator(session.DB, target)
	require.NoError(t, migrator.Init(ctx))
	_, err := migrator.Migrate(ctx)
	require.NoError(t, err)

	siteID := site.ID
	dao := model.NewExpectedRackGroupDAO(session)
	row, err := dao.Create(ctx, nil, model.ExpectedRackGroupCreateInput{
		ExpectedRackGroupID: uuid.New(), SiteID: siteID, RackGroupID: "group-1", Topology: "topology", CreatedBy: user.ID,
	})
	require.NoError(t, err)
	_, err = migrator.Rollback(ctx)
	require.NoError(t, err)
	group, err := migrator.Migrate(ctx)
	require.NoError(t, err)
	require.Len(t, group.Migrations, 1)
	stored, err := dao.Get(ctx, nil, row.ID, nil, false)
	require.NoError(t, err)
	require.Equal(t, row, stored)
	for _, tc := range []struct {
		name    string
		siteID  uuid.UUID
		groupID string
		wantErr bool
	}{
		{name: "same site rejects duplicate ID", siteID: siteID, groupID: "group-1", wantErr: true},
		{name: "another site can reuse ID", siteID: otherSite.ID, groupID: "group-1"},
		{name: "IDs are case sensitive", siteID: siteID, groupID: "GROUP-1"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := dao.Create(ctx, nil, model.ExpectedRackGroupCreateInput{
				ExpectedRackGroupID: uuid.New(), SiteID: tc.siteID, RackGroupID: tc.groupID, Topology: "topology", CreatedBy: user.ID,
			})
			if tc.wantErr {
				require.ErrorContains(t, err, "expected_rack_group_group_id_site_id_key")
			} else {
				require.NoError(t, err)
			}
		})
	}
	t.Run("racks cannot be SQL null", func(t *testing.T) {
		_, err := session.DB.ExecContext(ctx, "UPDATE expected_rack_group SET racks = NULL")
		require.ErrorContains(t, err, "not-null constraint")
	})
}
