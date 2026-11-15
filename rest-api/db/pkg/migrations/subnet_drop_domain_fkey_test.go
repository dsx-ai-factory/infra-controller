// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/migrate"
)

func TestSubnetDropDomainFkeyMigration(t *testing.T) {
	ctx := context.Background()
	session := util.GetTestDBSession(t, true)
	defer session.Close()
	model.TestSetupSchema(t, session)
	user := model.TestBuildUser(t, session, uuid.NewString(), "subnet-domain", []string{"FORGE_PROVIDER_ADMIN"})
	provider := model.TestBuildInfrastructureProvider(t, session, "provider", "subnet-domain", user)
	site := model.TestBuildSite(t, session, provider, "site", user)
	tenant := model.TestBuildTenant(t, session, "tenant", "subnet-domain", user)
	vpc := model.TestBuildVPC(t, session, "vpc", provider, tenant, site, nil, nil, nil, model.VpcStatusReady, user, nil)
	dao := model.NewSubnetDAO(session)
	subnet, err := dao.Create(ctx, nil, model.SubnetCreateInput{
		Name: "subnet", Org: "subnet-domain", SiteID: site.ID, VpcID: vpc.ID, TenantID: tenant.ID,
		PrefixLength: 24, Status: model.SubnetStatusReady, CreatedBy: user.ID,
	})
	require.NoError(t, err)

	// The schema built from the current model has neither the column nor the
	// key. Put both back the way a database from an earlier release has them.
	_, err = session.DB.ExecContext(ctx, `ALTER TABLE subnet
		ADD COLUMN domain_id uuid,
		ADD CONSTRAINT subnet_domain_id_fkey FOREIGN KEY (domain_id) REFERENCES domain (id)`)
	require.NoError(t, err)
	// A Domain that exists only in Core has no row in the REST domain table.
	coreDomainID := uuid.New()
	_, err = session.DB.ExecContext(ctx, "UPDATE subnet SET domain_id = ? WHERE id = ?", coreDomainID, subnet.ID)
	var pgErr *pgconn.PgError
	require.ErrorAs(t, err, &pgErr)
	require.Equal(t, "23503", pgErr.Code)
	require.Equal(t, "subnet_domain_id_fkey", pgErr.ConstraintName)

	target := migrate.NewMigrations()
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20261119044500" {
			target.Add(migration)
		}
	}
	require.Len(t, target.Sorted(), 1)
	migrator := migrate.NewMigrator(session.DB, target,
		migrate.WithTableName("subnet_drop_domain_fkey_migrations_test"),
		migrate.WithLocksTableName("subnet_drop_domain_fkey_migration_locks_test"),
		migrate.WithMarkAppliedOnSuccess(true))
	require.NoError(t, migrator.Init(ctx))
	_, err = migrator.Migrate(ctx)
	require.NoError(t, err)

	t.Run("an earlier release can write a Domain that REST does not store", func(t *testing.T) {
		_, err := session.DB.ExecContext(ctx, "UPDATE subnet SET domain_id = ? WHERE id = ?", coreDomainID, subnet.ID)
		require.NoError(t, err)
		var stored uuid.UUID
		require.NoError(t, session.DB.QueryRowContext(ctx, "SELECT domain_id FROM subnet WHERE id = ?", subnet.ID).Scan(&stored))
		assert.Equal(t, coreDomainID, stored)
	})
	t.Run("the Subnet is still read and updated through the current model", func(t *testing.T) {
		// Written here too, so this subtest passes when it runs alone.
		_, err := session.DB.ExecContext(ctx, "UPDATE subnet SET domain_id = ? WHERE id = ?", coreDomainID, subnet.ID)
		require.NoError(t, err)
		name := "renamed"
		updated, err := dao.Update(ctx, nil, model.SubnetUpdateInput{SubnetId: subnet.ID, Name: &name})
		require.NoError(t, err)
		assert.Equal(t, name, updated.Name)
		var stored uuid.UUID
		require.NoError(t, session.DB.QueryRowContext(ctx, "SELECT domain_id FROM subnet WHERE id = ?", subnet.ID).Scan(&stored))
		assert.Equal(t, coreDomainID, stored)
	})
	t.Run("running again with the key already gone succeeds", func(t *testing.T) {
		require.NoError(t, target.Sorted()[0].Up(ctx, session.DB))
	})
}
