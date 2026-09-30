// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(domainLifecycleUpMigration, domainLifecycleDownMigration)
}

func domainLifecycleUpMigration(ctx context.Context, db *bun.DB) error {
	// Detect duplicates before installing the index. Historical REST rows may
	// contain equivalent dotted/ASCII-case variants; never choose a winner or infer
	// Core ownership from a name. Match Core ASCII folding exactly (SQL lower
	// would additionally fold Unicode). Fail with the exact rows to reconcile.
	var tenantID, siteID, name, conflictingIDs string
	err := db.QueryRowContext(ctx, `
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
	// Only REST-owned reservations participate. Existing unowned inventory rows
	// remain untouched; a name never establishes Core ownership.
	_, err = db.ExecContext(ctx, `
		CREATE UNIQUE INDEX domain_owned_name_idx
		ON domain (tenant_id, site_id, translate(rtrim(hostname, '.'), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'))
		WHERE deleted IS NULL AND tenant_id IS NOT NULL AND site_id IS NOT NULL
		AND controller_domain_id IS NOT NULL
	`)
	if err != nil {
		return err
	}
	// Recovery claims are durable across workflow-worker restarts and expire
	// automatically. Legacy/inventory Domain rows are never eligible.
	_, err = db.ExecContext(ctx, `
		ALTER TABLE domain
		ADD COLUMN recovery_token uuid,
		ADD COLUMN recovery_lease_until timestamptz,
		ADD COLUMN recovery_next_at timestamptz,
		ADD COLUMN recovery_attempts integer NOT NULL DEFAULT 0
	`)
	if err != nil {
		return err
	}
	_, err = db.ExecContext(ctx, `
		CREATE INDEX domain_recovery_due_idx ON domain (recovery_next_at, updated, id)
		WHERE deleted IS NULL AND tenant_id IS NOT NULL AND site_id IS NOT NULL
		AND controller_domain_id IS NOT NULL
		AND status IN ('DomainStatusPending', 'DomainStatusRejecting', 'DomainStatusDeleting')
	`)
	if err != nil {
		return err
	}
	// The Subnet row is the serialization point for attach, inventory and
	// recovery. An intent retains both REST and Core VPC IDs plus the Core
	// segment version needed to fence a delayed RPC. A pending row must remain
	// retryable after a worker restart, never inferred from inventory alone.
	_, err = db.ExecContext(ctx, `
		ALTER TABLE subnet
		ADD COLUMN attach_intent_id uuid,
		ADD COLUMN attach_source_vpc_id uuid,
		ADD COLUMN attach_target_vpc_id uuid,
		ADD COLUMN attach_source_controller_vpc_id uuid,
		ADD COLUMN attach_target_controller_vpc_id uuid,
		ADD COLUMN attach_segment_version text,
		ADD COLUMN attach_recovery_token uuid,
		ADD COLUMN attach_lease_until timestamptz,
		ADD COLUMN attach_next_at timestamptz,
		ADD COLUMN attach_attempts integer NOT NULL DEFAULT 0
	`)
	if err != nil {
		return err
	}
	_, err = db.ExecContext(ctx, `
		CREATE INDEX subnet_attach_recovery_due_idx ON subnet (attach_next_at, updated, id)
		WHERE deleted IS NULL AND attach_intent_id IS NOT NULL
	`)
	if err != nil {
		return err
	}
	fmt.Print(" [up migration] Added owned Domain reservation uniqueness. ")
	return nil
}

func domainLifecycleDownMigration(_ context.Context, _ *bun.DB) error {
	// Preserving the index prevents old processes from creating duplicate
	// reservations during a rollback. A later deliberate migration can drop it.
	fmt.Print(" [down migration] No action taken")
	return nil
}
