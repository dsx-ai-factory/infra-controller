// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"
	"testing"
	"time"

	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/extra/bundebug"
	"github.com/uptrace/bun/migrate"
)

func TestMigrations(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, true)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test Migrations",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			migrator := migrate.NewMigrator(dbSession.DB, Migrations)
			migrator.Init(tt.args.ctx)
			_, err := migrator.Migrate(tt.args.ctx)
			assert.NoError(t, err)
			assertExpectedMachineInterfacesColumn(t, dbSession.DB)
		})
	}
}

func TestMachineLabelsGinOpsMigration(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()
	model.TestSetupSchema(t, dbSession)

	_, err := dbSession.DB.ExecContext(ctx, `
		CREATE INDEX machine_labels_gin_idx
		ON public.machine USING GIN (labels jsonb_path_ops)
	`)
	require.NoError(t, err)

	targetMigrations := migrate.NewMigrations()
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20260909000000" {
			targetMigrations.Add(migration)
		}
	}
	require.Len(t, targetMigrations.Sorted(), 1)

	migrator := migrate.NewMigrator(
		dbSession.DB,
		targetMigrations,
		migrate.WithTableName("machine_labels_gin_ops_migrations_test"),
		migrate.WithLocksTableName("machine_labels_gin_ops_migration_locks_test"),
		migrate.WithMarkAppliedOnSuccess(true),
	)
	require.NoError(t, migrator.Init(ctx))

	group, err := migrator.Migrate(ctx)
	require.NoError(t, err)
	require.Len(t, group.Migrations, 1)

	var indexDefinition string
	err = dbSession.DB.QueryRowContext(ctx, `
		SELECT pg_get_indexdef('public.machine_labels_gin_ops_idx'::regclass)
	`).Scan(&indexDefinition)
	require.NoError(t, err)
	require.Contains(t, indexDefinition, "USING gin (labels)")
	require.NotContains(t, indexDefinition, "jsonb_path_ops")

	var oldIndexName *string
	err = dbSession.DB.QueryRowContext(ctx, `
		SELECT to_regclass('public.machine_labels_gin_idx')::text
	`).Scan(&oldIndexName)
	require.NoError(t, err)
	require.Nil(t, oldIndexName)

	_, err = migrator.Rollback(ctx)
	require.NoError(t, err)

	err = dbSession.DB.QueryRowContext(ctx, `
		SELECT pg_get_indexdef('public.machine_labels_gin_idx'::regclass)
	`).Scan(&indexDefinition)
	require.NoError(t, err)
	require.Contains(t, indexDefinition, "jsonb_path_ops")

	var newIndexName *string
	err = dbSession.DB.QueryRowContext(ctx, `
		SELECT to_regclass('public.machine_labels_gin_ops_idx')::text
	`).Scan(&newIndexName)
	require.NoError(t, err)
	require.Nil(t, newIndexName)
}

func TestIPBlockSitePrefixUniqueMigration(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()
	model.TestSetupSchema(t, dbSession)

	org := "site-prefix-identity-provider"
	user := model.TestBuildUser(t, dbSession, uuid.NewString(), org, []string{authz.ProviderAdminRole})
	provider := model.TestBuildInfrastructureProvider(t, dbSession, "site-prefix-identity-provider", org, user)
	site := model.TestBuildSite(t, dbSession, provider, "site-prefix-identity-site", user)
	dao := model.NewIPBlockDAO(dbSession)
	sitePrefixID := uuid.New()

	create := func(name, prefix string, linkedSitePrefixID *uuid.UUID) (*model.IPBlock, error) {
		return dao.Create(ctx, nil, model.IPBlockCreateInput{
			Name:                     name,
			SiteID:                   site.ID,
			InfrastructureProviderID: provider.ID,
			SitePrefixID:             linkedSitePrefixID,
			RoutingType:              model.IPBlockRoutingTypeDatacenterOnly,
			Prefix:                   prefix,
			PrefixLength:             24,
			ProtocolVersion:          model.IPBlockProtocolVersionV4,
			Status:                   model.IPBlockStatusReady,
			CreatedBy:                &user.ID,
		})
	}

	firstNull, err := create("first-unlinked", "10.92.0.0", nil)
	require.NoError(t, err)
	secondNull, err := create("second-unlinked", "10.92.1.0", nil)
	require.NoError(t, err)
	linked, err := create("linked", "10.92.2.0", &sitePrefixID)
	require.NoError(t, err)

	var targetMigration migrate.Migration
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20260910140509" {
			targetMigration = migration
			break
		}
	}
	require.Equal(t, "ip_block_site_prefix_unique", targetMigration.Comment)

	targetMigrations := migrate.NewMigrations()
	targetMigrations.Add(targetMigration)
	migrator := migrate.NewMigrator(
		dbSession.DB,
		targetMigrations,
		migrate.WithTableName("ip_block_site_prefix_unique_migrations_test"),
		migrate.WithLocksTableName("ip_block_site_prefix_unique_migration_locks_test"),
		migrate.WithMarkAppliedOnSuccess(true),
	)
	require.NoError(t, migrator.Init(ctx))

	group, err := migrator.Migrate(ctx)
	require.NoError(t, err)
	require.Len(t, group.Migrations, 1)
	// A crash can leave the index committed before Bun records the migration.
	// Replaying the callback must converge on the same schema.
	require.NoError(t, ipBlockSitePrefixUniqueUpMigration(ctx, dbSession.DB))

	for _, id := range []uuid.UUID{firstNull.ID, secondNull.ID} {
		persisted, err := dao.GetByID(ctx, nil, id, nil)
		require.NoError(t, err)
		require.Nil(t, persisted.SitePrefixID)
	}
	thirdNull, err := create("third-unlinked", "10.92.3.0", nil)
	require.NoError(t, err)
	require.Nil(t, thirdNull.SitePrefixID)

	var indexDefinition string
	err = dbSession.DB.QueryRowContext(ctx, `
		SELECT pg_get_indexdef('public.ip_block_site_prefix_id_key'::regclass)
	`).Scan(&indexDefinition)
	require.NoError(t, err)
	require.Contains(t, indexDefinition, "WHERE (site_prefix_id IS NOT NULL)")

	require.NoError(t, dao.Delete(ctx, nil, linked.ID))
	_, err = create("replacement", "10.92.4.0", &sitePrefixID)
	require.ErrorContains(t, err, "ip_block_site_prefix_id_key")

	_, err = migrator.Rollback(ctx)
	require.ErrorIs(t, err, errIPBlockSitePrefixIdentityRollback)

	statuses, err := migrator.MigrationsWithStatus(ctx)
	require.NoError(t, err)
	require.Len(t, statuses, 1)
	require.True(t, statuses[0].IsApplied())

	_, err = create("replacement-after-rollback", "10.92.5.0", &sitePrefixID)
	require.ErrorContains(t, err, "ip_block_site_prefix_id_key")
}

func TestVpcSlaacEnabledMigration(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()
	model.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipUser := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, []string{authz.ProviderAdminRole})
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipUser)
	tenantOrg := "test-tenant-org"
	tenantUser := model.TestBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{authz.TenantAdminRole})
	tenant := model.TestBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := model.TestBuildSite(t, dbSession, ip, "test-site", ipUser)
	vpc := model.TestBuildVPC(t, dbSession, "test-vpc", ip, tenant, site, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusReady, tenantUser, nil)

	// Fresh installs create `slaac_enabled` before this migration runs.
	require.NoError(t, vpcSlaacEnabledUpMigration(ctx, dbSession.DB))
	var isNullable, columnDefault string
	err := dbSession.DB.QueryRowContext(ctx, `
		SELECT is_nullable, column_default
		FROM information_schema.columns
		WHERE table_schema = 'public' AND table_name = 'vpc' AND column_name = 'slaac_enabled'
	`).Scan(&isNullable, &columnDefault)
	require.NoError(t, err)
	require.Equal(t, "NO", isNullable)
	require.Equal(t, "false", columnDefault)

	// Recreate the immediate predecessor schema while retaining a realistic old row.
	_, err = dbSession.DB.ExecContext(ctx, `ALTER TABLE vpc DROP COLUMN slaac_enabled`)
	require.NoError(t, err)

	var targetMigration migrate.Migration
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20260817184424" {
			targetMigration = migration
			break
		}
	}
	require.Equal(t, "vpc_slaac_enabled", targetMigration.Comment)

	targetMigrations := migrate.NewMigrations()
	targetMigrations.Add(targetMigration)
	migrator := migrate.NewMigrator(
		dbSession.DB,
		targetMigrations,
		migrate.WithTableName("vpc_slaac_enabled_migrations_test"),
		migrate.WithLocksTableName("vpc_slaac_enabled_migration_locks_test"),
		migrate.WithMarkAppliedOnSuccess(true),
	)
	require.NoError(t, migrator.Init(ctx))

	group, err := migrator.Migrate(ctx)
	require.NoError(t, err)
	require.Len(t, group.Migrations, 1)

	persisted, err := model.NewVpcDAO(dbSession).GetByID(ctx, nil, vpc.ID, nil)
	require.NoError(t, err)
	require.False(t, persisted.SlaacEnabled)

	persisted, err = model.NewVpcDAO(dbSession).Update(ctx, nil, model.VpcUpdateInput{
		VpcID:        vpc.ID,
		SlaacEnabled: cutil.GetPtr(true),
	})
	require.NoError(t, err)
	require.True(t, persisted.SlaacEnabled)

	_, err = migrator.Rollback(ctx)
	require.ErrorIs(t, err, errVpcSlaacEnabledRollback)

	statuses, err := migrator.MigrationsWithStatus(ctx)
	require.NoError(t, err)
	require.Len(t, statuses, 1)
	require.True(t, statuses[0].IsApplied())

	persisted, err = model.NewVpcDAO(dbSession).GetByID(ctx, nil, vpc.ID, nil)
	require.NoError(t, err)
	require.True(t, persisted.SlaacEnabled)

	// Fresh installs already contain `slaac_enabled` before this migration runs.
	// Reapplying the up callback must preserve its data.
	require.NoError(t, vpcSlaacEnabledUpMigration(ctx, dbSession.DB))
	persisted, err = model.NewVpcDAO(dbSession).GetByID(ctx, nil, vpc.ID, nil)
	require.NoError(t, err)
	require.True(t, persisted.SlaacEnabled)
}

func TestDomainOwnershipMigration(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()

	model.TestSetupSchema(t, dbSession)
	user := model.TestBuildUser(t, dbSession, uuid.NewString(), "test-org", []string{authz.TenantAdminRole})
	domain, err := model.NewDomainDAO(dbSession).Create(ctx, nil, model.DomainCreateInput{
		Hostname:  "legacy.example.com",
		Org:       "test-org",
		Status:    model.DomainStatusReady,
		CreatedBy: user.ID,
	})
	require.NoError(t, err)

	_, err = dbSession.DB.ExecContext(ctx, `ALTER TABLE domain DROP COLUMN tenant_id, DROP COLUMN site_id`)
	require.NoError(t, err)
	require.NoError(t, domainOwnershipUpMigration(ctx, dbSession.DB))

	var nullableOwnershipColumns int
	err = dbSession.DB.QueryRowContext(ctx, `
		SELECT COUNT(*)
		FROM information_schema.columns
		WHERE table_schema = 'public'
		  AND table_name = 'domain'
		  AND column_name IN ('tenant_id', 'site_id')
		  AND is_nullable = 'YES'
	`).Scan(&nullableOwnershipColumns)
	require.NoError(t, err)
	assert.Equal(t, 2, nullableOwnershipColumns)

	var legacyTenantID, legacySiteID *uuid.UUID
	err = dbSession.DB.QueryRowContext(ctx, `SELECT tenant_id, site_id FROM domain WHERE id = ?`, domain.ID).Scan(&legacyTenantID, &legacySiteID)
	require.NoError(t, err)
	assert.Nil(t, legacyTenantID)
	assert.Nil(t, legacySiteID)

	tenantID := uuid.New()
	siteID := uuid.New()
	_, err = dbSession.DB.ExecContext(ctx, `UPDATE domain SET tenant_id = ?, site_id = ? WHERE id = ?`, tenantID, siteID, domain.ID)
	require.NoError(t, err)
	require.NoError(t, domainOwnershipUpMigration(ctx, dbSession.DB))
	require.NoError(t, domainOwnershipDownMigration(ctx, dbSession.DB))

	var persistedTenantID, persistedSiteID uuid.UUID
	err = dbSession.DB.QueryRowContext(ctx, `SELECT tenant_id, site_id FROM domain WHERE id = ?`, domain.ID).Scan(&persistedTenantID, &persistedSiteID)
	require.NoError(t, err)
	assert.Equal(t, tenantID, persistedTenantID)
	assert.Equal(t, siteID, persistedSiteID)

	var ownershipIndexCount int
	err = dbSession.DB.QueryRowContext(ctx, `
		SELECT COUNT(*)
		FROM pg_indexes
		WHERE schemaname = 'public'
		  AND tablename = 'domain'
		  AND indexname = 'domain_tenant_site_idx'
	`).Scan(&ownershipIndexCount)
	require.NoError(t, err)
	assert.Equal(t, 1, ownershipIndexCount)
}

var (
	domainLifecycleRecoveryColumns = []string{"recovery_token", "recovery_lease_until", "recovery_next_at", "recovery_attempts"}
	domainLifecycleAttachColumns   = []string{
		"attach_intent_id", "attach_source_vpc_id", "attach_target_vpc_id",
		"attach_source_controller_vpc_id", "attach_target_controller_vpc_id",
		"attach_segment_version", "attach_recovery_token", "attach_lease_until",
		"attach_next_at", "attach_attempts",
	}
	domainLifecycleIndexes = []string{"domain_owned_name_idx", "domain_recovery_due_idx", "subnet_attach_recovery_due_idx"}
)

// newDomainLifecycleMigrator runs only the lifecycle migration through the
// same Bun migrator options as the production db init_migrate command.
func newDomainLifecycleMigrator(t *testing.T, ctx context.Context, dbSession *db.Session) *migrate.Migrator {
	t.Helper()
	var targetMigration migrate.Migration
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20260930180000" {
			targetMigration = migration
			break
		}
	}
	require.Equal(t, "domain_lifecycle", targetMigration.Comment)
	targetMigrations := migrate.NewMigrations()
	targetMigrations.Add(targetMigration)
	migrator := migrate.NewMigrator(
		dbSession.DB,
		targetMigrations,
		migrate.WithTableName("domain_lifecycle_migrations_test"),
		migrate.WithLocksTableName("domain_lifecycle_migration_locks_test"),
		migrate.WithMarkAppliedOnSuccess(true),
	)
	require.NoError(t, migrator.Init(ctx))
	return migrator
}

func domainLifecycleMigrationApplied(t *testing.T, ctx context.Context, migrator *migrate.Migrator) bool {
	t.Helper()
	statuses, err := migrator.MigrationsWithStatus(ctx)
	require.NoError(t, err)
	require.Len(t, statuses, 1)
	return statuses[0].IsApplied()
}

func countDomainLifecycleColumns(t *testing.T, ctx context.Context, dbSession *db.Session, table string, columns []string) int {
	t.Helper()
	var count int
	err := dbSession.DB.QueryRowContext(ctx, `
		SELECT COUNT(*) FROM information_schema.columns
		WHERE table_schema = 'public' AND table_name = ? AND column_name IN (?)
	`, table, bun.In(columns)).Scan(&count)
	require.NoError(t, err)
	return count
}

// assertDomainLifecycleSchema checks the exact converged schema: every column,
// the zero defaults on the attempt counters, and every index definition.
func assertDomainLifecycleSchema(t *testing.T, ctx context.Context, dbSession *db.Session) {
	t.Helper()
	require.Equal(t, len(domainLifecycleRecoveryColumns), countDomainLifecycleColumns(t, ctx, dbSession, "domain", domainLifecycleRecoveryColumns))
	require.Equal(t, len(domainLifecycleAttachColumns), countDomainLifecycleColumns(t, ctx, dbSession, "subnet", domainLifecycleAttachColumns))
	for table, column := range map[string]string{"domain": "recovery_attempts", "subnet": "attach_attempts"} {
		var isNullable string
		var columnDefault sql.NullString
		err := dbSession.DB.QueryRowContext(ctx, `
			SELECT is_nullable, column_default FROM information_schema.columns
			WHERE table_schema = 'public' AND table_name = ? AND column_name = ?
		`, table, column).Scan(&isNullable, &columnDefault)
		require.NoError(t, err)
		assert.Equal(t, "NO", isNullable, "%s.%s", table, column)
		assert.Equal(t, "0", columnDefault.String, "%s.%s", table, column)
	}
	definitions := map[string]string{}
	rows, err := dbSession.DB.QueryContext(ctx, `
		SELECT indexname, indexdef FROM pg_indexes
		WHERE schemaname = 'public' AND indexname IN (?)
	`, bun.In(domainLifecycleIndexes))
	require.NoError(t, err)
	defer rows.Close()
	for rows.Next() {
		var name, definition string
		require.NoError(t, rows.Scan(&name, &definition))
		definitions[name] = definition
	}
	require.NoError(t, rows.Err())
	require.Len(t, definitions, len(domainLifecycleIndexes))
	assert.Contains(t, definitions["domain_owned_name_idx"], "CREATE UNIQUE INDEX domain_owned_name_idx ON public.domain USING btree (tenant_id, site_id, translate(rtrim((hostname)::text, '.'::text), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'::text, 'abcdefghijklmnopqrstuvwxyz'::text))")
	assert.Contains(t, definitions["domain_owned_name_idx"], "WHERE ((deleted IS NULL) AND (tenant_id IS NOT NULL) AND (site_id IS NOT NULL) AND (controller_domain_id IS NOT NULL))")
	assert.Contains(t, definitions["domain_recovery_due_idx"], "(recovery_next_at, updated, id)")
	assert.Contains(t, definitions["domain_recovery_due_idx"], "'DomainStatusRejecting'")
	assert.Contains(t, definitions["subnet_attach_recovery_due_idx"], "(attach_next_at, updated, id) WHERE ((deleted IS NULL) AND (attach_intent_id IS NOT NULL))")
}

// dropDomainLifecycleSchema recreates the immediate predecessor schema of an
// upgraded install, which never had the lifecycle columns or indexes.
func dropDomainLifecycleSchema(t *testing.T, ctx context.Context, dbSession *db.Session) {
	t.Helper()
	_, err := dbSession.DB.ExecContext(ctx, `DROP INDEX IF EXISTS domain_owned_name_idx, domain_recovery_due_idx, subnet_attach_recovery_due_idx`)
	require.NoError(t, err)
	_, err = dbSession.DB.ExecContext(ctx, `ALTER TABLE domain DROP COLUMN recovery_token, DROP COLUMN recovery_lease_until, DROP COLUMN recovery_next_at, DROP COLUMN recovery_attempts`)
	require.NoError(t, err)
	_, err = dbSession.DB.ExecContext(ctx, `ALTER TABLE subnet DROP COLUMN attach_intent_id, DROP COLUMN attach_source_vpc_id, DROP COLUMN attach_target_vpc_id, DROP COLUMN attach_source_controller_vpc_id, DROP COLUMN attach_target_controller_vpc_id, DROP COLUMN attach_segment_version, DROP COLUMN attach_recovery_token, DROP COLUMN attach_lease_until, DROP COLUMN attach_next_at, DROP COLUMN attach_attempts`)
	require.NoError(t, err)
	require.Equal(t, 0, countDomainLifecycleColumns(t, ctx, dbSession, "domain", domainLifecycleRecoveryColumns))
	require.Equal(t, 0, countDomainLifecycleColumns(t, ctx, dbSession, "subnet", domainLifecycleAttachColumns))
}

func TestDomainLifecycleMigrationFreshInstall(t *testing.T) {
	ctx := context.Background()
	// A brand-new database: the initial migration creates domain and subnet
	// from the current models, which already include every lifecycle column.
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()

	migrator := migrate.NewMigrator(dbSession.DB, Migrations, migrate.WithMarkAppliedOnSuccess(true))
	require.NoError(t, migrator.Init(ctx))
	group, err := migrator.Migrate(ctx)
	require.NoError(t, err)
	require.False(t, group.IsZero())
	statuses, err := migrator.MigrationsWithStatus(ctx)
	require.NoError(t, err)
	require.Empty(t, statuses.Unapplied())
	assertDomainLifecycleSchema(t, ctx, dbSession)

	// Re-running init_migrate on the migrated database is a no-op.
	require.NoError(t, migrator.Init(ctx))
	group, err = migrator.Migrate(ctx)
	require.NoError(t, err)
	require.True(t, group.IsZero())
}

func TestDomainLifecycleMigrationUpgradePreservesRows(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()
	model.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipUser := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, []string{authz.ProviderAdminRole})
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipUser)
	tenantOrg := "test-tenant-org"
	tenantUser := model.TestBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{authz.TenantAdminRole})
	tenant := model.TestBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := model.TestBuildSite(t, dbSession, ip, "test-site", ipUser)
	vpc := model.TestBuildVPC(t, dbSession, "test-vpc", ip, tenant, site, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusReady, tenantUser, nil)
	ipBlock := model.TestBuildIPBlock(t, dbSession, "test-block", site, tenant, model.IPBlockRoutingTypeDatacenterOnly, "192.0.2.0", 24, model.IPBlockProtocolVersionV4)
	subnet := model.TestBuildSubnet(t, dbSession, "test-subnet", tenant, vpc, nil, ipBlock, model.SubnetStatusReady, tenantUser)

	domainDAO := model.NewDomainDAO(dbSession)
	legacy, err := domainDAO.Create(ctx, nil, model.DomainCreateInput{
		Hostname: "legacy.example.com", Org: tenantOrg, Status: model.DomainStatusReady, CreatedBy: tenantUser.ID,
	})
	require.NoError(t, err)
	owned, err := domainDAO.Create(ctx, nil, model.DomainCreateInput{
		Hostname: "owned.example.com", Org: tenantOrg, TenantID: &tenant.ID, SiteID: &site.ID,
		ControllerDomainID: cutil.GetPtr(uuid.New()), Status: model.DomainStatusReady, CreatedBy: tenantUser.ID,
	})
	require.NoError(t, err)

	dropDomainLifecycleSchema(t, ctx, dbSession)
	migrator := newDomainLifecycleMigrator(t, ctx, dbSession)
	group, err := migrator.Migrate(ctx)
	require.NoError(t, err)
	require.Len(t, group.Migrations, 1)
	require.True(t, domainLifecycleMigrationApplied(t, ctx, migrator))
	assertDomainLifecycleSchema(t, ctx, dbSession)

	// Services start after migrations; read back through a new session so no
	// statement prepared against the predecessor schema is reused.
	migratedSession := util.GetTestDBSession(t, false)
	defer migratedSession.Close()
	domainDAO = model.NewDomainDAO(migratedSession)

	// Existing rows survive, receive zero attempts and no recovery or attach state.
	for _, domain := range []*model.Domain{legacy, owned} {
		persisted, err := domainDAO.GetByID(ctx, nil, domain.ID, nil)
		require.NoError(t, err)
		assert.Equal(t, domain.Hostname, persisted.Hostname)
		assert.Equal(t, domain.TenantID, persisted.TenantID)
		assert.Equal(t, domain.SiteID, persisted.SiteID)
		assert.Equal(t, domain.ControllerDomainID, persisted.ControllerDomainID)
		assert.Equal(t, 0, persisted.RecoveryAttempts)
		assert.Nil(t, persisted.RecoveryToken)
		assert.Nil(t, persisted.RecoveryNextAt)
	}
	persistedSubnet, err := model.NewSubnetDAO(migratedSession).GetByID(ctx, nil, subnet.ID, nil)
	require.NoError(t, err)
	assert.Equal(t, vpc.ID, persistedSubnet.VpcID)
	assert.Equal(t, 0, persistedSubnet.AttachAttempts)
	assert.Nil(t, persistedSubnet.AttachIntentID)

	// The unique index enforces owned-name uniqueness with Core ASCII folding.
	_, err = domainDAO.Create(ctx, nil, model.DomainCreateInput{
		Hostname: "OWNED.example.com.", Org: tenantOrg, TenantID: &tenant.ID, SiteID: &site.ID,
		ControllerDomainID: cutil.GetPtr(uuid.New()), Status: model.DomainStatusPending, CreatedBy: tenantUser.ID,
	})
	require.ErrorContains(t, err, "domain_owned_name_idx")

	// A later rerun of init_migrate sees the migration as applied.
	group, err = migrator.Migrate(ctx)
	require.NoError(t, err)
	require.True(t, group.IsZero())
}

func TestDomainLifecycleMigrationRetriesAfterPartialFailure(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()
	// Fresh model columns plus the unique index that the previous
	// non-transactional migration committed before failing on ADD COLUMN.
	// The leftover deliberately differs from the intended definition.
	model.TestSetupSchema(t, dbSession)
	_, err := dbSession.DB.ExecContext(ctx, `CREATE UNIQUE INDEX domain_owned_name_idx ON domain (tenant_id, site_id, hostname)`)
	require.NoError(t, err)

	migrator := newDomainLifecycleMigrator(t, ctx, dbSession)
	group, err := migrator.Migrate(ctx)
	require.NoError(t, err)
	require.Len(t, group.Migrations, 1)
	require.True(t, domainLifecycleMigrationApplied(t, ctx, migrator))
	assertDomainLifecycleSchema(t, ctx, dbSession)

	// Replaying the callback (crash before Bun records it) converges again.
	require.NoError(t, domainLifecycleUpMigration(ctx, dbSession.DB))
	assertDomainLifecycleSchema(t, ctx, dbSession)
}

func TestDomainLifecycleMigrationRejectsDuplicateOwnedDomains(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()
	model.TestSetupSchema(t, dbSession)

	user := model.TestBuildUser(t, dbSession, uuid.NewString(), "test-org", []string{authz.TenantAdminRole})
	tenantID, siteID := uuid.New(), uuid.New()
	dropDomainLifecycleSchema(t, ctx, dbSession)
	insert := func(hostname string) uuid.UUID {
		id := uuid.New()
		_, err := dbSession.DB.ExecContext(ctx, `
			INSERT INTO domain (id, hostname, org, tenant_id, site_id, controller_domain_id, status, created_by)
			VALUES (?, ?, 'test-org', ?, ?, ?, ?, ?)
		`, id, hostname, tenantID, siteID, uuid.New(), model.DomainStatusReady, user.ID)
		require.NoError(t, err)
		return id
	}
	first := insert("dup.example.com")
	second := insert("DUP.Example.com.")

	migrator := newDomainLifecycleMigrator(t, ctx, dbSession)
	_, err := migrator.Migrate(ctx)
	require.ErrorContains(t, err, "cannot enforce owned Domain uniqueness")
	require.ErrorContains(t, err, `normalized name "dup.example.com"`)
	require.ErrorContains(t, err, first.String())
	require.ErrorContains(t, err, second.String())
	// Nothing is recorded or partially applied.
	require.False(t, domainLifecycleMigrationApplied(t, ctx, migrator))
	require.Equal(t, 0, countDomainLifecycleColumns(t, ctx, dbSession, "domain", domainLifecycleRecoveryColumns))
	require.Equal(t, 0, countDomainLifecycleColumns(t, ctx, dbSession, "subnet", domainLifecycleAttachColumns))

	// Once the operator reconciles ownership, the same migration succeeds.
	_, err = dbSession.DB.ExecContext(ctx, `UPDATE domain SET deleted = current_timestamp WHERE id = ?`, second)
	require.NoError(t, err)
	_, err = migrator.Migrate(ctx)
	require.NoError(t, err)
	require.True(t, domainLifecycleMigrationApplied(t, ctx, migrator))
	assertDomainLifecycleSchema(t, ctx, dbSession)
}

func TestDomainLifecycleMigrationRejectsIncompatibleColumns(t *testing.T) {
	ctx := context.Background()
	for _, tc := range []struct {
		name      string
		alter     string
		errSubstr string
	}{
		{"wrong type", `ALTER TABLE domain ALTER COLUMN recovery_token TYPE text`, "incompatible existing column domain.recovery_token: type text"},
		{"nullable counter", `ALTER TABLE subnet ALTER COLUMN attach_attempts DROP NOT NULL`, "incompatible existing column subnet.attach_attempts: nullable"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dbSession := util.GetTestDBSession(t, true)
			defer dbSession.Close()
			model.TestSetupSchema(t, dbSession)
			_, err := dbSession.DB.ExecContext(ctx, tc.alter)
			require.NoError(t, err)

			migrator := newDomainLifecycleMigrator(t, ctx, dbSession)
			_, err = migrator.Migrate(ctx)
			require.ErrorContains(t, err, tc.errSubstr)
			require.False(t, domainLifecycleMigrationApplied(t, ctx, migrator))
			// The transaction rolled back the index creation as well.
			var indexes int
			err = dbSession.DB.QueryRowContext(ctx, `SELECT COUNT(*) FROM pg_indexes WHERE schemaname = 'public' AND indexname IN (?)`, bun.In(domainLifecycleIndexes)).Scan(&indexes)
			require.NoError(t, err)
			require.Equal(t, 0, indexes)
		})
	}
}

func Test_vpcProviderIDUpMigration(t *testing.T) {
	ctx := context.Background()

	// Ensure test DB
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	defer dbSession.Close()

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	tnu := model.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := model.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	site1 := model.TestBuildSite(t, dbSession, ip, "Test Site 1", ipu)
	site2 := model.TestBuildSite(t, dbSession, ip, "Test Site 2", ipu)

	nsg1 := model.TestBuildNetworkSecurityGroup(t, dbSession, "Test NSG1", tn, site1)
	nsg2 := model.TestBuildNetworkSecurityGroup(t, dbSession, "Test NSG2", tn, site2)

	vpc1 := model.TestBuildVPC(t, dbSession, "Test VPC 1", ip, tn, site1, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusReady, tnu, &nsg1.ID)
	vpc2 := model.TestBuildVPC(t, dbSession, "Test VPC 2", ip, tn, site2, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusReady, tnu, &nsg2.ID)

	// Delete VPC2 and Site2 to test that the migration will not fail because of deleted rows
	_, err := dbSession.DB.NewDelete().Model(vpc2).WherePK().Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewDelete().Model(site2).WherePK().Exec(ctx)
	require.NoError(t, err)

	// Allow infrastructure_provider_id to be null
	_, err = dbSession.DB.Exec("ALTER TABLE vpc ALTER COLUMN infrastructure_provider_id DROP NOT NULL")
	assert.Nil(t, err)

	// Set infrastructure_provider_id to null
	_, err = dbSession.DB.NewUpdate().Table("public.vpc").Set("infrastructure_provider_id = ?", nil).Where("id = ?", vpc1.ID).Exec(ctx)
	assert.Nil(t, err)
	_, err = dbSession.DB.NewUpdate().Table("public.vpc").Set("infrastructure_provider_id = ?", nil).Where("id = ?", vpc2.ID).Exec(ctx)
	assert.Nil(t, err)

	var updatedVpc1, updatedVpc2 model.Vpc
	var emptyUUID uuid.UUID

	err = dbSession.DB.NewSelect().Model(&updatedVpc1).Where("id = ?", vpc1.ID).Scan(ctx)
	assert.NoError(t, err)

	assert.Equal(t, emptyUUID, updatedVpc1.InfrastructureProviderID)

	err = dbSession.DB.NewSelect().Model(&updatedVpc2).Where("id = ?", vpc2.ID).WhereAllWithDeleted().Scan(ctx)
	assert.NoError(t, err)

	assert.Equal(t, emptyUUID, updatedVpc2.InfrastructureProviderID)

	// Call up migration function
	err = vpcProviderIDUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Check that InfrastructureProviderID has been populated
	err = dbSession.DB.NewSelect().Model(&updatedVpc1).Where("id = ?", vpc1.ID).Scan(ctx)
	assert.NoError(t, err)

	assert.Equal(t, ip.ID, updatedVpc1.InfrastructureProviderID)

	err = dbSession.DB.NewSelect().Model(&updatedVpc2).Where("id = ?", vpc2.ID).WhereAllWithDeleted().Scan(ctx)
	assert.NoError(t, err)

	assert.Equal(t, ip.ID, updatedVpc2.InfrastructureProviderID)
}

func Test_subnetSiteIDUpMigration(t *testing.T) {
	ctx := context.Background()

	// Ensure test DB
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	defer dbSession.Close()

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	tnu := model.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := model.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	site1 := model.TestBuildSite(t, dbSession, ip, "Test Site 1", ipu)
	vpc1 := model.TestBuildVPC(t, dbSession, "Test VPC 1", ip, tn, site1, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusReady, tnu, nil)
	ipv4Block1 := model.TestBuildIPBlock(t, dbSession, "Test IPv4 Block 1", site1, tn, model.IPBlockRoutingTypeDatacenterOnly, "192.0.2.0", 24, model.IPBlockProtocolVersionV4)
	subnet1 := model.TestBuildSubnet(t, dbSession, "Test Subnet 1", tn, vpc1, nil, ipv4Block1, model.SubnetStatusPending, tnu)

	site2 := model.TestBuildSite(t, dbSession, ip, "Test Site 2", ipu)
	vpc2 := model.TestBuildVPC(t, dbSession, "Test VPC 2", ip, tn, site2, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusReady, tnu, nil)
	ipv4Block2 := model.TestBuildIPBlock(t, dbSession, "Test IPv4 Block 2", site2, tn, model.IPBlockRoutingTypeDatacenterOnly, "192.0.3.0", 24, model.IPBlockProtocolVersionV4)
	subnet2 := model.TestBuildSubnet(t, dbSession, "Test Subnet 2", tn, vpc2, nil, ipv4Block2, model.SubnetStatusPending, tnu)

	// Delete the second set to test that the migration will not fail because of deleted rows
	_, err := dbSession.DB.NewDelete().Model(subnet2).WherePK().Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewDelete().Model(ipv4Block2).WherePK().Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewDelete().Model(vpc2).WherePK().Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewDelete().Model(site2).WherePK().Exec(ctx)
	require.NoError(t, err)

	// Allow site_id to be null
	_, err = dbSession.DB.Exec("ALTER TABLE subnet ALTER COLUMN site_id DROP NOT NULL")
	assert.Nil(t, err)

	// Set site_id to null
	_, err = dbSession.DB.NewUpdate().Table("public.subnet").Set("site_id = ?", nil).Where("id = ?", subnet1.ID).Exec(ctx)
	assert.Nil(t, err)
	_, err = dbSession.DB.NewUpdate().Table("public.subnet").Set("site_id = ?", nil).Where("id = ?", subnet2.ID).Exec(ctx)
	assert.Nil(t, err)

	var updatedSubnet1, updatedSubnet2 model.Subnet
	var emptyUUID uuid.UUID

	err = dbSession.DB.NewSelect().Model(&updatedSubnet1).Where("id = ?", subnet1.ID).Scan(ctx)
	assert.NoError(t, err)
	assert.Equal(t, emptyUUID, updatedSubnet1.SiteID)
	err = dbSession.DB.NewSelect().Model(&updatedSubnet2).WhereAllWithDeleted().Where("id = ?", subnet2.ID).Scan(ctx)
	assert.NoError(t, err)
	assert.Equal(t, emptyUUID, updatedSubnet2.SiteID)

	// Call up migration function
	err = subnetSiteIDUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Check that Site ID has been populated
	err = dbSession.DB.NewSelect().Model(&updatedSubnet1).Where("id = ?", subnet1.ID).Scan(ctx)
	assert.NoError(t, err)
	assert.Equal(t, site1.ID, updatedSubnet1.SiteID)

	err = dbSession.DB.NewSelect().Model(&updatedSubnet2).WhereAllWithDeleted().Where("id = ?", subnet2.ID).Scan(ctx)
	assert.NoError(t, err)
	assert.Equal(t, site2.ID, updatedSubnet2.SiteID)
}

func Test_machineInstanceTypeIDUpMigration(t *testing.T) {
	ctx := context.Background()

	// Ensure test DB
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	defer dbSession.Close()

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)

	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	instanceType := model.TestBuildInstanceType(t, dbSession, "Test Instance Type", ip, site, ipu)

	// Create Machine without Instance Type
	machine := model.TestBuildMachine(t, dbSession, ip, site, nil, nil)
	require.Nil(t, machine.InstanceTypeID)

	// Create Machine/Instance Type association
	model.TestBuildMachineInstanceType(t, dbSession, machine, instanceType)

	// Call up migration function
	err := machineInstanceTypeIDUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Check that Instance Type ID has been populated for Machine
	var updatedMachine model.Machine
	err = dbSession.DB.NewSelect().Model(&updatedMachine).Where("id = ?", machine.ID).Scan(ctx)
	assert.NoError(t, err)

	require.NotNil(t, updatedMachine.InstanceTypeID)
	assert.Equal(t, instanceType.ID, *updatedMachine.InstanceTypeID)
}

func Test_machineControllerMachineIDUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// setup schemas
	// Machine is the baremetal server that sits in the datacenter
	type MachineOlder struct {
		bun.BaseModel `bun:"table:machine,alias:m"`

		ID                       uuid.UUID                     `bun:"type:uuid,pk"`
		InfrastructureProviderID uuid.UUID                     `bun:"infrastructure_provider_id,type:uuid,notnull"`
		InfrastructureProvider   *model.InfrastructureProvider `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
		SiteID                   uuid.UUID                     `bun:"site_id,type:uuid,notnull"`
		Site                     *model.Site                   `bun:"rel:belongs-to,join:site_id=id"`
		InstanceTypeID           *uuid.UUID                    `bun:"instance_type_id,type:uuid"`
		InstanceType             *model.InstanceType           `bun:"rel:belongs-to,join:instance_type_id=id"`
		ControllerMachineID      uuid.UUID                     `bun:"controller_machine_id,type:uuid,notnull"`
		ControllerMachineType    *string                       `bun:"controller_machine_type"`
		HwSkuDeviceType          *string                       `bun:"hw_sku_device_type"`
		Metadata                 map[string]interface{}        `bun:"metadata,type:jsonb,json_use_number"`
		DefaultMacAddress        *string                       `bun:"default_mac_address"`
		IsAssigned               bool                          `bun:"is_assigned,notnull"`
		Status                   string                        `bun:"status,notnull"`
		IsMissingOnSite          bool                          `bun:"is_missing_on_site,notnull"`
		Created                  time.Time                     `bun:"created,nullzero,notnull,default:current_timestamp"`
		Updated                  time.Time                     `bun:"updated,nullzero,notnull,default:current_timestamp"`
		Deleted                  *time.Time                    `bun:"deleted,soft_delete"`
	}

	type MachineNewer struct {
		bun.BaseModel `bun:"table:machine,alias:m"`

		ID                       uuid.UUID                     `bun:"type:uuid,pk"`
		InfrastructureProviderID uuid.UUID                     `bun:"infrastructure_provider_id,type:uuid,notnull"`
		InfrastructureProvider   *model.InfrastructureProvider `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
		SiteID                   uuid.UUID                     `bun:"site_id,type:uuid,notnull"`
		Site                     *model.Site                   `bun:"rel:belongs-to,join:site_id=id"`
		InstanceTypeID           *uuid.UUID                    `bun:"instance_type_id,type:uuid"`
		InstanceType             *model.InstanceType           `bun:"rel:belongs-to,join:instance_type_id=id"`
		ControllerMachineID      string                        `bun:"controller_machine_id,notnull"`
		ControllerMachineType    *string                       `bun:"controller_machine_type"`
		HwSkuDeviceType          *string                       `bun:"hw_sku_device_type"`
		Metadata                 map[string]interface{}        `bun:"metadata,type:jsonb,json_use_number"`
		DefaultMacAddress        *string                       `bun:"default_mac_address"`
		IsAssigned               bool                          `bun:"is_assigned,notnull"`
		Status                   string                        `bun:"status,notnull"`
		IsMissingOnSite          bool                          `bun:"is_missing_on_site,notnull"`
		Created                  time.Time                     `bun:"created,nullzero,notnull,default:current_timestamp"`
		Updated                  time.Time                     `bun:"updated,nullzero,notnull,default:current_timestamp"`
		Deleted                  *time.Time                    `bun:"deleted,soft_delete"`
	}

	// create Allocation table
	err := dbSession.DB.ResetModel(context.Background(), (*model.Allocation)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Tenant)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Site)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InstanceType)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Vpc)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Machine)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*model.OperatingSystem)(nil))
	assert.Nil(t, err)
	// create OperatingSystemSiteAssociation table
	err = dbSession.DB.ResetModel(context.Background(), (*model.OperatingSystemSiteAssociation)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*model.User)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Instance)(nil))
	assert.Nil(t, err)
	// create InfiniBandPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfiniBandPartition)(nil))
	assert.Nil(t, err)
	// create InfiniBandInterface table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfiniBandInterface)(nil))
	assert.Nil(t, err)
	// create old Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*MachineOlder)(nil))
	assert.Nil(t, err)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)

	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	instanceType := model.TestBuildInstanceType(t, dbSession, "Test Instance Type", ip, site, ipu)

	defMacAddr := "00:1B:44:11:3A:B7"
	controllerMachineType := "machineTypeTest"

	// Create Machines
	mcs := []uuid.UUID{}
	for i := 0; i < 5; i++ {
		mc := uuid.New()
		mcs = append(mcs, mc)

		machine := &MachineOlder{
			ID:                       uuid.New(),
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			ControllerMachineID:      mc,
			ControllerMachineType:    &controllerMachineType,
			Metadata:                 nil,
			DefaultMacAddress:        &defMacAddr,
			Status:                   model.MachineStatusInitializing,
		}

		if instanceType != nil {
			machine.InstanceTypeID = &instanceType.ID
		}

		_, err = dbSession.DB.NewInsert().Model(machine).Exec(context.Background())
		assert.Nil(t, err)
	}

	// Call up migration function
	err = machineControllerMachineIDUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// GetAll machines, and verify that controller_machine_id matches
	nms := []MachineNewer{}

	err = dbSession.DB.NewSelect().Model(&nms).Scan(ctx)
	assert.Nil(t, err)

	assert.Equal(t, len(mcs), len(nms))
	assert.Nil(t, err)
	for i, m := range nms {
		assert.Equal(t, m.ControllerMachineID, mcs[i].String())
	}
}

func Test_ipBlockBlockSizeRenameUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// setup schemas
	// create Tenant table
	err := dbSession.DB.ResetModel(context.Background(), (*model.Tenant)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Site)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)
	tenant := model.TestBuildTenant(t, dbSession, "testTen", "testOrg", ipu)
	ipb := model.TestBuildIPBlock(t, dbSession, "test-ipb", site, tenant, model.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, model.IPBlockProtocolVersionV4)

	// rename column prefixLength back to block_size for up migration testing
	_, err = dbSession.DB.Exec("ALTER TABLE ip_block RENAME COLUMN prefix_length TO block_size")
	assert.NoError(t, err)

	// Call up migration function
	err = ipBlockBlockSizeRenameUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// GetAll ipblocks and verify
	ipbDAO := model.NewIPBlockDAO(dbSession)
	ipbs, tot, err := ipbDAO.GetAll(context.Background(), nil, model.IPBlockFilterInput{}, paginator.PageInput{}, nil)
	assert.Equal(t, tot, len(ipbs))
	assert.Equal(t, 1, tot)
	assert.Nil(t, err)
	assert.Equal(t, ipb.PrefixLength, ipbs[0].PrefixLength)
	assert.Equal(t, ipb.Name, ipbs[0].Name)
}

func Test_subnetIPBlockSizeRenameUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// setup schemas
	// create Tenant table
	err := dbSession.DB.ResetModel(context.Background(), (*model.Tenant)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Site)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Vpc)(nil))
	assert.Nil(t, err)
	// create domain table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Domain)(nil))
	assert.Nil(t, err)
	// create ipblock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*model.User)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Subnet)(nil))
	assert.Nil(t, err)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)
	tenant := model.TestBuildTenant(t, dbSession, "testTen", "testOrg", ipu)
	vpc := model.TestBuildVPC(t, dbSession, "testvpc", ip, tenant, site, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusProvisioning, ipu, nil)
	ipb := model.TestBuildIPBlock(t, dbSession, "test-ipb", site, tenant, model.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, model.IPBlockProtocolVersionV4)
	subnet := model.TestBuildSubnet(t, dbSession, "testsubnet", tenant, vpc, nil, ipb, model.SubnetStatusProvisioning, ipu)

	// rename column prefixLength back to block_size for up migration testing
	_, err = dbSession.DB.Exec("ALTER TABLE subnet RENAME COLUMN prefix_length TO ip_block_size")
	assert.NoError(t, err)

	// Call up migration function
	err = subnetIPBlockSizeRenameUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// GetAll ipblocks and verify
	subnetDAO := model.NewSubnetDAO(dbSession)
	subnets, tot, err := subnetDAO.GetAll(context.Background(), nil, model.SubnetFilterInput{}, paginator.PageInput{}, []string{})
	assert.Equal(t, tot, len(subnets))
	assert.Equal(t, 1, tot)
	assert.Nil(t, err)
	assert.Equal(t, subnet.PrefixLength, subnets[0].PrefixLength)
	assert.Equal(t, subnet.Name, subnets[0].Name)
}

func Test_renameInstanceSubnetToInterfaceUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, true)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// Setup schemas
	model.TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}
	tnu := model.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := model.TestBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu)

	st := model.TestBuildSite(t, dbSession, ip, "test-site", ipu)
	al := model.TestBuildAllocation(t, dbSession, "test-allocation", st, tn, ipu)
	it := model.TestBuildInstanceType(t, dbSession, "test-instance-type", ip, st, ipu)
	model.TestBuildAllocationConstraint(t, dbSession, al, it, nil, 40, ipu)

	vpc := model.TestBuildVPC(t, dbSession, "test-vpc", ip, tn, st, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusProvisioning, ipu, nil)
	ipb := model.TestBuildIPBlock(t, dbSession, "test-ipb", st, tn, model.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, model.IPBlockProtocolVersionV4)
	sb := model.TestBuildSubnet(t, dbSession, "test-subnet", tn, vpc, nil, ipb, model.SubnetStatusProvisioning, ipu)
	os := model.TestBuildOperatingSystem(t, dbSession, "test-os", tn, model.OperatingSystemStatusReady, ipu)

	ifcCount := 30
	for i := 0; i < ifcCount; i++ {
		m := model.TestBuildMachine(t, dbSession, ip, st, it, nil)
		ins := model.TestBuildInstance(t, dbSession, fmt.Sprintf("test-instance-%d", i), tn, ip, st, it, vpc, m, os)
		model.TestBuildInterface(t, dbSession, ins, &sb.ID, nil, true, model.InterfaceStatusProvisioning)
	}

	// rename column prefixLength back to block_size for up migration testing
	_, err := dbSession.DB.Exec("ALTER TABLE IF EXISTS interface RENAME TO instance_subnet")
	assert.NoError(t, err)

	// Call up migration function
	err = renameInstanceSubnetToInterfaceUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// GetAll Interfaces and verify
	ifcDAO := model.NewInterfaceDAO(dbSession)
	_, tot, err := ifcDAO.GetAll(context.Background(), nil, model.InterfaceFilterInput{}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	assert.NoError(t, err)
	assert.Equal(t, ifcCount, tot)
}

func Test_createAndPopulateTenantSiteUpMigrationfunc(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, true)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// Setup schemas
	model.TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnRoles := []string{authz.TenantAdminRole}

	tnu1 := model.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg1, tnRoles)
	tn1 := model.TestBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)

	tnu2 := model.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg2, tnRoles)
	tn2 := model.TestBuildTenant(t, dbSession, "test-tenant", tnOrg2, tnu2)

	st := model.TestBuildSite(t, dbSession, ip, "test-site", ipu)

	al1 := model.TestBuildAllocation(t, dbSession, "test-instance-allocation", st, tn1, ipu)
	it := model.TestBuildInstanceType(t, dbSession, "test-instance-type", ip, st, ipu)
	model.TestBuildAllocationConstraint(t, dbSession, al1, it, nil, 40, ipu)

	al2 := model.TestBuildAllocation(t, dbSession, "test-ip-allocation", st, tn2, ipu)
	ipb := model.TestBuildIPBlock(t, dbSession, "test-ipb", st, tn2, model.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, model.IPBlockProtocolVersionV4)
	model.TestBuildAllocationConstraint(t, dbSession, al2, nil, ipb, 40, ipu)

	// Delete TenantSite table
	_, err := dbSession.DB.NewDropTable().IfExists().Model((*model.TenantSite)(nil)).Exec(ctx)
	assert.NoError(t, err)

	// Call up migration function
	err = createAndPopulateTenantSiteUpMigrationfunc(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Check that 2 TenantSite entries were create
	tsDAO := model.NewTenantSiteDAO(dbSession)
	_, tot, err := tsDAO.GetAll(context.Background(), nil, model.TenantSiteFilterInput{}, paginator.PageInput{}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 2, tot)
}

func Test_siteSshHostnameRenameUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, true)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// Setup schemas
	model.TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	// rename column serial_console_hostname back to ssh_hostname for up migration testing
	_, err := dbSession.DB.Exec("ALTER TABLE site RENAME COLUMN serial_console_hostname TO ssh_hostname")
	assert.NoError(t, err)

	// Call up migration function
	err = siteSshHostnameRenameUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Get site and verify
	siteDAO := model.NewSiteDAO(dbSession)
	sites, tot, err := siteDAO.GetAll(context.Background(), nil, model.SiteFilterInput{}, paginator.PageInput{}, nil)
	assert.Equal(t, tot, len(sites))
	assert.Equal(t, 1, tot)
	assert.Nil(t, err)
	assert.Equal(t, site.SerialConsoleHostname, sites[0].SerialConsoleHostname)
	assert.Equal(t, site.Name, sites[0].Name)

	// Call up migration function again, here serialConsoleHostname already exists - this should be ok
	err = siteSshHostnameRenameUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)
}

// TODO: evaluate if this test centered around a 2023 migration is still needed/
func Test_alterMachineIDUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, true)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// Setup schemas
	model.TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	st := model.TestBuildSite(t, dbSession, ip, "test-site", ipu)
	it := model.TestBuildInstanceType(t, dbSession, "test-instance-type", ip, st, ipu)

	// Create Machines
	mCount := 30
	for i := 0; i < mCount; i++ {
		m := model.TestBuildMachine(t, dbSession, ip, st, it, nil)
		assert.NotEqual(t, m.ID, m.ControllerMachineID)
	}

	// Drop foreign key constraints
	_, err := dbSession.DB.Exec("ALTER TABLE machine_capability DROP CONSTRAINT machine_capability_machine_id_fkey")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_instance_type DROP CONSTRAINT machine_instance_type_machine_id_fkey")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_interface DROP CONSTRAINT machine_interface_machine_id_fkey")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE instance DROP CONSTRAINT instance_machine_id_fkey")
	assert.NoError(t, err)

	// Drop expected_machine foreign key constraint if it exists
	_, err = dbSession.DB.Exec("ALTER TABLE expected_machine DROP CONSTRAINT IF EXISTS expected_machine_machine_id_fkey")
	assert.NoError(t, err)

	// Change back all Machine ID to UUID
	_, err = dbSession.DB.Exec("ALTER TABLE machine_capability ALTER COLUMN machine_id TYPE uuid USING machine_id::uuid")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_instance_type ALTER COLUMN machine_id TYPE uuid USING machine_id::uuid")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_interface ALTER COLUMN machine_id TYPE uuid USING machine_id::uuid")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE instance ALTER COLUMN machine_id TYPE uuid USING machine_id::uuid")
	assert.NoError(t, err)

	// Change expected_machine.machine_id to UUID if the column exists
	_, err = dbSession.DB.Exec("ALTER TABLE expected_machine ALTER COLUMN machine_id TYPE uuid USING machine_id::uuid")
	if err != nil {
		// Column might not exist in older schema versions, that's okay
		fmt.Printf("Note: expected_machine.machine_id column might not exist yet: %v\n", err)
	}

	_, err = dbSession.DB.Exec("ALTER TABLE machine ALTER COLUMN id TYPE uuid USING id::uuid")
	assert.NoError(t, err)

	// Add back foreign key constraint
	_, err = dbSession.DB.Exec("ALTER TABLE machine_capability ADD CONSTRAINT machine_capability_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES public.machine(id)")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_instance_type ADD CONSTRAINT machine_instance_type_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES public.machine(id)")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_interface ADD CONSTRAINT machine_interface_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES public.machine(id)")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE instance ADD CONSTRAINT instance_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES public.machine(id)")
	assert.NoError(t, err)

	// Note: We do NOT add back expected_machine foreign key constraint here because:
	// - expected_machine.machine_id is TEXT (from TestSetupSchema)
	// - machine.id is UUID at this point (we just converted it back)
	// - The types are mismatched, so the constraint would fail
	// - The alterMachineIDUpMigration will convert machine.id to TEXT
	// - After migration, we can add the constraint if needed

	// Run migration
	err = alterMachineIDUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Check that all Machine IDs are now strings
	mDAO := model.NewMachineDAO(dbSession)
	ms, tot, err := mDAO.GetAll(context.Background(), nil, model.MachineFilterInput{}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	assert.NoError(t, err)
	assert.Equal(t, tot, mCount)

	for _, m := range ms {
		assert.Equal(t, m.ID, m.ControllerMachineID)
	}
}

func Test_operatingSystemImageAttributeUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// setup schemas
	// create Tenant table
	err := dbSession.DB.ResetModel(context.Background(), (*model.Tenant)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Site)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Vpc)(nil))
	assert.Nil(t, err)
	// create domain table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Domain)(nil))
	assert.Nil(t, err)
	// create ipblock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*model.User)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Subnet)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*model.OperatingSystem)(nil))
	assert.Nil(t, err)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)
	tenant := model.TestBuildTenant(t, dbSession, "testTen", "testOrg", ipu)
	vpc := model.TestBuildVPC(t, dbSession, "testvpc", ip, tenant, site, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusProvisioning, ipu, nil)
	ipb := model.TestBuildIPBlock(t, dbSession, "test-ipb", site, tenant, model.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, model.IPBlockProtocolVersionV4)
	_ = model.TestBuildSubnet(t, dbSession, "testsubnet", tenant, vpc, nil, ipb, model.SubnetStatusProvisioning, ipu)
	_ = model.TestBuildOperatingSystem(t, dbSession, "testos", tenant, model.OperatingSystemStatusProvisioning, ipu)

	// Call up migration function
	err = operatingSystemImageAttributeUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// GetAll operating systems and verify
	osDAO := model.NewOperatingSystemDAO(dbSession)
	oss, tos, err := osDAO.GetAll(context.Background(), nil, model.OperatingSystemFilterInput{}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	assert.Equal(t, tos, len(oss))
	assert.Equal(t, 1, tos)
	assert.Nil(t, err)
	assert.Equal(t, "iPXE", oss[0].Type)
}

func Test_tenantConfigUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// Create Tenant table
	err := dbSession.DB.ResetModel(context.Background(), (*model.Tenant)(nil))
	assert.Nil(t, err)

	// Tenant.Config is scan-only, so ResetModel does not create its column.
	// Recreate the historical schema before exercising the migration.
	_, err = dbSession.DB.Exec("ALTER TABLE tenant ADD COLUMN config jsonb")
	assert.NoError(t, err)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrg3 := "test-tenant-org-3"

	tenant1 := model.TestBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, ipu)
	tenant2 := model.TestBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, ipu)
	tenant3 := model.TestBuildTenant(t, dbSession, "test-tenant-3", tnOrg3, ipu)

	// Simulate legacy rows with NULL configs prior to the migration.
	_, err = dbSession.DB.Exec("UPDATE tenant SET config = NULL WHERE id IN (?)", bun.In([]uuid.UUID{tenant1.ID, tenant2.ID}))
	assert.NoError(t, err)

	// Seed one tenant with a pre-existing non-NULL config that must survive the migration.
	existingConfig := map[string]interface{}{
		"targetedInstanceCreation": true,
		"enableSshAccess":          false,
	}
	_, err = dbSession.DB.NewUpdate().
		Table("tenant").
		Set("config = ?", existingConfig).
		Where("id = ?", tenant3.ID).
		Exec(ctx)
	assert.NoError(t, err)

	// Call up migration function
	err = tenantConfigUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// The migration backfills NULL configs with an empty JSON object and leaves
	// pre-existing non-NULL values untouched.
	type tenantConfigRow struct {
		ID     uuid.UUID              `bun:"id"`
		Config map[string]interface{} `bun:"config,type:jsonb"`
	}

	var rows []tenantConfigRow
	err = dbSession.DB.NewSelect().
		Table("tenant").
		Column("id", "config").
		Where("id IN (?)", bun.In([]uuid.UUID{tenant1.ID, tenant2.ID, tenant3.ID})).
		Scan(ctx, &rows)
	assert.NoError(t, err)
	require.Len(t, rows, 3)

	configByID := make(map[uuid.UUID]map[string]interface{}, len(rows))
	for _, row := range rows {
		configByID[row.ID] = row.Config
	}

	assert.Equal(t, map[string]interface{}{}, configByID[tenant1.ID])
	assert.Equal(t, map[string]interface{}{}, configByID[tenant2.ID])
	assert.Equal(t, existingConfig, configByID[tenant3.ID])

	// The migration also establishes the column's schema contract: it sets the
	// '{}'::jsonb default and a NOT NULL constraint. Exercise both behaviors so a
	// regression that drops either is caught.

	// A new row that omits config must receive the '{}'::jsonb default.
	defaultTenant := model.TestBuildTenant(t, dbSession, "test-tenant-default", "test-tenant-org-default", ipu)

	var defaultRow tenantConfigRow
	err = dbSession.DB.NewSelect().
		Table("tenant").
		Column("id", "config").
		Where("id = ?", defaultTenant.ID).
		Scan(ctx, &defaultRow)
	assert.NoError(t, err)
	assert.Equal(t, map[string]interface{}{}, defaultRow.Config)

	// Explicitly writing NULL must violate the NOT NULL constraint.
	_, err = dbSession.DB.NewUpdate().
		Table("tenant").
		Set("config = NULL").
		Where("id = ?", defaultTenant.ID).
		Exec(ctx)
	assert.Error(t, err)
}

// TestDpuExtensionServiceDpuTargetMigration verifies that historical Helm services receive the
// compatibility target while Pod services remain unchanged.
func Test_dpuExtensionServiceDpuTargetMigration(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()

	// Recreate the minimal historical table shape before dpu_target existed.
	_, err := dbSession.DB.ExecContext(ctx, `
		CREATE TABLE dpu_extension_service (
			id TEXT PRIMARY KEY,
			service_type TEXT NOT NULL
		)
	`)
	require.NoError(t, err)

	// Seed both historical service types to prove the migration backfills only Helm rows.
	_, err = dbSession.DB.ExecContext(ctx, `
		INSERT INTO dpu_extension_service (id, service_type)
		VALUES ('historical-helm', 'DpfHelmChart'), ('historical-pod', 'KubernetesPod')
	`)
	require.NoError(t, err)

	// Apply the migration directly so the test exercises its forward schema contract.
	require.NoError(t, dpuExtensionServiceDpuTargetUpMigration(ctx, dbSession.DB))

	// Historical Helm rows adopt AllActive while Kubernetes Pod rows keep no target.
	var helmTarget string
	err = dbSession.DB.QueryRowContext(ctx, `
		SELECT dpu_target FROM dpu_extension_service WHERE id = 'historical-helm'
	`).Scan(&helmTarget)
	require.NoError(t, err)
	assert.Equal(t, "AllActive", helmTarget)

	var podTarget *string
	err = dbSession.DB.QueryRowContext(ctx, `
		SELECT dpu_target FROM dpu_extension_service WHERE id = 'historical-pod'
	`).Scan(&podTarget)
	require.NoError(t, err)
	assert.Nil(t, podTarget)
}
