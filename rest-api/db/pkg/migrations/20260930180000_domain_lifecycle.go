// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"
	"slices"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(domainLifecycleUpMigration, domainLifecycleDownMigration)
}

// domainLifecycleColumn is a column that the lifecycle migration owns. Fresh
// installs create it from the current Bun model while upgraded installs add it
// here, so each column accepts the type produced by either path.
type domainLifecycleColumn struct {
	table   string
	name    string
	types   []string
	notNull bool
}

var domainLifecycleColumns = []domainLifecycleColumn{
	{table: "domain", name: "recovery_token", types: []string{"uuid"}},
	{table: "domain", name: "recovery_lease_until", types: []string{"timestamp with time zone"}},
	{table: "domain", name: "recovery_next_at", types: []string{"timestamp with time zone"}},
	{table: "domain", name: "recovery_attempts", types: []string{"integer", "bigint"}, notNull: true},
	{table: "subnet", name: "attach_intent_id", types: []string{"uuid"}},
	{table: "subnet", name: "attach_source_vpc_id", types: []string{"uuid"}},
	{table: "subnet", name: "attach_target_vpc_id", types: []string{"uuid"}},
	{table: "subnet", name: "attach_source_controller_vpc_id", types: []string{"uuid"}},
	{table: "subnet", name: "attach_target_controller_vpc_id", types: []string{"uuid"}},
	{table: "subnet", name: "attach_segment_version", types: []string{"text", "character varying"}},
	{table: "subnet", name: "attach_recovery_token", types: []string{"uuid"}},
	{table: "subnet", name: "attach_lease_until", types: []string{"timestamp with time zone"}},
	{table: "subnet", name: "attach_next_at", types: []string{"timestamp with time zone"}},
	{table: "subnet", name: "attach_attempts", types: []string{"integer", "bigint"}, notNull: true},
}

func domainLifecycleUpMigration(ctx context.Context, db *bun.DB) error {
	// Fresh installs create the domain and subnet tables from the current Bun
	// models, which already contain the recovery and attach columns, while
	// upgraded installs reach this migration without them. Every statement runs
	// in one transaction and is idempotent, so a fresh install, an upgrade and a
	// retry after a failed earlier attempt all converge on the same schema.
	err := db.RunInTx(ctx, &sql.TxOptions{}, func(ctx context.Context, tx bun.Tx) error {
		// Detect duplicates before installing the index. Historical REST rows may
		// contain equivalent dotted/ASCII-case variants; never choose a winner or infer
		// Core ownership from a name. Match Core ASCII folding exactly (SQL lower
		// would additionally fold Unicode). Fail with the exact rows to reconcile.
		var tenantID, siteID, name, conflictingIDs string
		err := tx.QueryRowContext(ctx, `
			SELECT tenant_id::text, site_id::text, translate(rtrim(hostname, '.'), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'),
				string_agg(id::text, ', ' ORDER BY id::text)
			FROM domain
			WHERE deleted IS NULL AND tenant_id IS NOT NULL AND site_id IS NOT NULL
			AND controller_domain_id IS NOT NULL
			GROUP BY tenant_id, site_id, translate(rtrim(hostname, '.'), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz')
			HAVING count(*) > 1 LIMIT 1
		`).Scan(&tenantID, &siteID, &name, &conflictingIDs)
		if err != nil && err != sql.ErrNoRows {
			return fmt.Errorf("inspect preexisting owned Domain reservations: %w", err)
		}
		if err == nil {
			return fmt.Errorf("cannot enforce owned Domain uniqueness: tenant %s site %s normalized name %q has conflicting REST row IDs [%s]; reconcile existing ownership before migration", tenantID, siteID, name, conflictingIDs)
		}
		// An earlier non-transactional attempt of this migration could commit
		// any of these indexes before failing. Recreate them so their definition
		// is exactly the one below rather than trusting a same-named leftover.
		if _, err := tx.ExecContext(ctx, `
			DROP INDEX IF EXISTS domain_owned_name_idx, domain_recovery_due_idx, subnet_attach_recovery_due_idx
		`); err != nil {
			return err
		}
		// Only REST-owned reservations participate. Existing unowned inventory rows
		// remain untouched; a name never establishes Core ownership.
		if _, err := tx.ExecContext(ctx, `
			CREATE UNIQUE INDEX domain_owned_name_idx
			ON domain (tenant_id, site_id, translate(rtrim(hostname, '.'), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'))
			WHERE deleted IS NULL AND tenant_id IS NOT NULL AND site_id IS NOT NULL
			AND controller_domain_id IS NOT NULL
		`); err != nil {
			return err
		}
		// Recovery claims are durable across workflow-worker restarts and expire
		// automatically. Legacy/inventory Domain rows are never eligible.
		if _, err := tx.ExecContext(ctx, `
			ALTER TABLE domain
			ADD COLUMN IF NOT EXISTS recovery_token uuid,
			ADD COLUMN IF NOT EXISTS recovery_lease_until timestamptz,
			ADD COLUMN IF NOT EXISTS recovery_next_at timestamptz,
			ADD COLUMN IF NOT EXISTS recovery_attempts integer NOT NULL DEFAULT 0,
			ALTER COLUMN recovery_attempts SET DEFAULT 0
		`); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, `
			CREATE INDEX domain_recovery_due_idx ON domain (recovery_next_at, updated, id)
			WHERE deleted IS NULL AND tenant_id IS NOT NULL AND site_id IS NOT NULL
			AND controller_domain_id IS NOT NULL
			AND status IN ('DomainStatusPending', 'DomainStatusRejecting', 'DomainStatusDeleting')
		`); err != nil {
			return err
		}
		// The Subnet row is the serialization point for attach, inventory and
		// recovery. An intent retains both REST and Core VPC IDs plus the Core
		// segment version needed to fence a delayed RPC. A pending row must remain
		// retryable after a worker restart, never inferred from inventory alone.
		if _, err := tx.ExecContext(ctx, `
			ALTER TABLE subnet
			ADD COLUMN IF NOT EXISTS attach_intent_id uuid,
			ADD COLUMN IF NOT EXISTS attach_source_vpc_id uuid,
			ADD COLUMN IF NOT EXISTS attach_target_vpc_id uuid,
			ADD COLUMN IF NOT EXISTS attach_source_controller_vpc_id uuid,
			ADD COLUMN IF NOT EXISTS attach_target_controller_vpc_id uuid,
			ADD COLUMN IF NOT EXISTS attach_segment_version text,
			ADD COLUMN IF NOT EXISTS attach_recovery_token uuid,
			ADD COLUMN IF NOT EXISTS attach_lease_until timestamptz,
			ADD COLUMN IF NOT EXISTS attach_next_at timestamptz,
			ADD COLUMN IF NOT EXISTS attach_attempts integer NOT NULL DEFAULT 0,
			ALTER COLUMN attach_attempts SET DEFAULT 0
		`); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, `
			CREATE INDEX subnet_attach_recovery_due_idx ON subnet (attach_next_at, updated, id)
			WHERE deleted IS NULL AND attach_intent_id IS NOT NULL
		`); err != nil {
			return err
		}
		// ADD COLUMN IF NOT EXISTS silently keeps a same-named column of any
		// shape. Refuse to record success over a column this code cannot use.
		return verifyDomainLifecycleColumns(ctx, tx)
	})
	if err != nil {
		return err
	}
	fmt.Print(" [up migration] Added owned Domain reservation uniqueness and durable recovery state. ")
	return nil
}

func verifyDomainLifecycleColumns(ctx context.Context, tx bun.Tx) error {
	for _, column := range domainLifecycleColumns {
		var dataType, isNullable string
		err := tx.QueryRowContext(ctx, `
			SELECT data_type, is_nullable
			FROM information_schema.columns
			WHERE table_schema = current_schema() AND table_name = ? AND column_name = ?
		`, column.table, column.name).Scan(&dataType, &isNullable)
		if err != nil {
			return fmt.Errorf("inspect %s.%s: %w", column.table, column.name, err)
		}
		if !slices.Contains(column.types, dataType) {
			return fmt.Errorf("incompatible existing column %s.%s: type %s, expected one of %v", column.table, column.name, dataType, column.types)
		}
		if column.notNull && isNullable != "NO" {
			return fmt.Errorf("incompatible existing column %s.%s: nullable, expected NOT NULL", column.table, column.name)
		}
	}
	return nil
}

func domainLifecycleDownMigration(_ context.Context, _ *bun.DB) error {
	// Preserving the index prevents old processes from creating duplicate
	// reservations during a rollback. A later deliberate migration can drop it.
	fmt.Print(" [down migration] No action taken")
	return nil
}
