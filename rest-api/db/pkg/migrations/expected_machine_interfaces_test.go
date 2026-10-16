// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/migrate"
)

func TestExpectedMachineInterfacesMigration(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()
	model.TestSetupSchema(t, dbSession)

	user := model.TestBuildUser(t, dbSession, "interfaces-migration-user", "interfaces-migration-org", []string{"admin"})
	provider := model.TestBuildInfrastructureProvider(t, dbSession, "interfaces-migration-provider", "interfaces-migration-org", user)
	site := model.TestBuildSite(t, dbSession, provider, "interfaces-migration-site", user)
	dao := model.NewExpectedMachineDAO(dbSession)
	existing, err := dao.Create(ctx, nil, model.ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "02:00:00:00:00:01",
		ChassisSerialNumber: "existing-machine",
		CreatedBy:           user.ID,
	})
	require.NoError(t, err)

	_, err = dbSession.DB.ExecContext(ctx, `ALTER TABLE expected_machine DROP COLUMN interfaces`)
	require.NoError(t, err)

	targetMigrations := migrate.NewMigrations()
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20261005122641" {
			targetMigrations.Add(migration)
		}
	}
	require.Len(t, targetMigrations.Sorted(), 1)

	migrator := migrate.NewMigrator(
		dbSession.DB,
		targetMigrations,
		migrate.WithTableName("expected_machine_interfaces_migrations_test"),
		migrate.WithLocksTableName("expected_machine_interfaces_migration_locks_test"),
		migrate.WithMarkAppliedOnSuccess(true),
	)
	require.NoError(t, migrator.Init(ctx))
	group, err := migrator.Migrate(ctx)
	require.NoError(t, err)
	require.Len(t, group.Migrations, 1)
	assertExpectedMachineInterfacesColumn(t, dbSession.DB)

	got, err := dao.Get(ctx, nil, existing.ID, nil, false)
	require.NoError(t, err)
	assert.NotNil(t, got.Interfaces)
	assert.Empty(t, got.Interfaces)

	created, err := dao.Create(ctx, nil, model.ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "02:00:00:00:00:02",
		ChassisSerialNumber: "new-machine",
		Interfaces: []model.ExpectedMachineInterface{{
			MacAddress: "02:00:00:00:00:09",
			NicType:    stringPointer("CX9"),
			FixedIP:    stringPointer("192.0.2.9"),
		}},
		CreatedBy: user.ID,
	})
	require.NoError(t, err)
	assert.Equal(t, []model.ExpectedMachineInterface{{
		MacAddress: "02:00:00:00:00:09",
		NicType:    stringPointer("CX9"),
		FixedIP:    stringPointer("192.0.2.9"),
	}}, created.Interfaces)
}

func assertExpectedMachineInterfacesColumn(t *testing.T, database *bun.DB) {
	t.Helper()
	var dataType string
	var isNullable string
	var columnDefault string
	err := database.QueryRowContext(context.Background(), `
		SELECT data_type, is_nullable, column_default
		FROM information_schema.columns
		WHERE table_schema = 'public'
		  AND table_name = 'expected_machine'
		  AND column_name = 'interfaces'
	`).Scan(&dataType, &isNullable, &columnDefault)
	require.NoError(t, err)
	assert.Equal(t, "jsonb", dataType)
	assert.Equal(t, "NO", isNullable)
	assert.Equal(t, "'[]'::jsonb", columnDefault)
}

func stringPointer(value string) *string {
	return &value
}
