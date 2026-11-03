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
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/migrate"
)

func TestMachineForceDeletionAttemptsMigration(t *testing.T) {
	ctx := context.Background()
	session := util.GetTestDBSession(t, true)
	defer session.Close()
	model.TestSetupSchema(t, session)
	user := model.TestBuildUser(t, session, uuid.NewString(), "deletion-migration", []string{"FORGE_PROVIDER_ADMIN"})
	provider := model.TestBuildInfrastructureProvider(t, session, "provider", "deletion-migration", user)
	site := model.TestBuildSite(t, session, provider, "site", user)
	dao := model.NewMachineDAO(session)
	ordinary := model.TestBuildMachine(t, session, provider, site, nil, nil)
	retained := model.TestBuildMachine(t, session, provider, site, nil, nil)
	_, err := session.DB.ExecContext(ctx, "UPDATE machine SET is_force_deletion_requested = true WHERE id = ?", retained.ID)
	require.NoError(t, err)
	_, err = session.DB.ExecContext(ctx, `ALTER TABLE machine DROP COLUMN is_force_deletion_complete, DROP COLUMN force_deletion_workflow_id`)
	require.NoError(t, err)

	target := migrate.NewMigrations()
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20261103194500" {
			target.Add(migration)
		}
	}
	require.Len(t, target.Sorted(), 1)
	migrator := migrate.NewMigrator(session.DB, target,
		migrate.WithTableName("force_deletion_attempt_migrations_test"),
		migrate.WithLocksTableName("force_deletion_attempt_migration_locks_test"),
		migrate.WithMarkAppliedOnSuccess(true))
	require.NoError(t, migrator.Init(ctx))
	_, err = migrator.Migrate(ctx)
	require.NoError(t, err)

	for _, tc := range []struct {
		name      string
		machine   *model.Machine
		requested bool
	}{
		{"ordinary Machine remains ordinary", ordinary, false},
		{"old retained marker is not completion proof", retained, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			stored, err := dao.GetByID(ctx, nil, tc.machine.ID, nil, false)
			require.NoError(t, err)
			assert.Equal(t, tc.requested, stored.IsForceDeletionRequested)
			assert.False(t, stored.IsForceDeletionComplete)
			assert.Nil(t, stored.ForceDeletionWorkflowID)
		})
	}
	t.Run("older writer may omit the new fields", func(t *testing.T) {
		oldWriter := *ordinary
		oldWriter.ID = uuid.NewString()
		oldWriter.ControllerMachineID = oldWriter.ID
		_, err := session.DB.NewInsert().Model(&oldWriter).ExcludeColumn("is_force_deletion_complete", "force_deletion_workflow_id").Exec(ctx)
		require.NoError(t, err)
		stored, err := dao.GetByID(ctx, nil, oldWriter.ID, nil, false)
		require.NoError(t, err)
		assert.False(t, stored.IsForceDeletionComplete)
		assert.Nil(t, stored.ForceDeletionWorkflowID)
	})
	t.Run("older marker-only writer can leave stale completion", func(t *testing.T) {
		_, err := dao.Update(ctx, nil, model.MachineUpdateInput{MachineID: retained.ID, IsForceDeletionComplete: cutil.GetPtr(true)})
		require.NoError(t, err)
		_, err = session.DB.ExecContext(ctx, "UPDATE machine SET is_force_deletion_requested = false WHERE id = ?", retained.ID)
		require.NoError(t, err)
		stored, err := dao.GetByID(ctx, nil, retained.ID, nil, false)
		require.NoError(t, err)
		assert.False(t, stored.IsForceDeletionRequested)
		assert.True(t, stored.IsForceDeletionComplete, "callers must require the requested marker as well as completion")
	})

}
