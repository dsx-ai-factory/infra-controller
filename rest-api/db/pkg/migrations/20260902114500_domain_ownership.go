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
	Migrations.MustRegister(domainOwnershipUpMigration, domainOwnershipDownMigration)
}

func domainOwnershipUpMigration(ctx context.Context, db *bun.DB) error {
	err := db.RunInTx(ctx, &sql.TxOptions{}, func(ctx context.Context, tx bun.Tx) error {
		if _, err := tx.ExecContext(ctx, `ALTER TABLE domain ADD COLUMN IF NOT EXISTS tenant_id UUID`); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, `ALTER TABLE domain ADD COLUMN IF NOT EXISTS site_id UUID`); err != nil {
			return err
		}
		_, err := tx.ExecContext(ctx, `
			CREATE INDEX IF NOT EXISTS domain_tenant_site_idx
			ON domain (tenant_id, site_id)
			WHERE deleted IS NULL
		`)
		return err
	})
	if err != nil {
		return err
	}

	fmt.Print(" [up migration] Added nullable Tenant and Site ownership to Domain records. ")
	return nil
}

func domainOwnershipDownMigration(_ context.Context, _ *bun.DB) error {
	// Preserve ownership data. Older binaries ignore these additive columns.
	fmt.Print(" [down migration] No action taken")
	return nil
}
