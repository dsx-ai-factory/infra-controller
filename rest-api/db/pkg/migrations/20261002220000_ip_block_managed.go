// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func ipBlockManagedUpMigration(ctx context.Context, db *bun.DB) error {
	// Fresh databases create the column from the model before reaching this
	// migration. Existing writers that omit it create NICo-managed blocks.
	_, err := db.ExecContext(ctx, `
		ALTER TABLE ip_block ADD COLUMN IF NOT EXISTS managed BOOLEAN NOT NULL DEFAULT TRUE;
		ALTER TABLE ip_block ALTER COLUMN managed SET DEFAULT TRUE;
		UPDATE ip_block SET managed = FALSE
		WHERE tenant_id IS NOT NULL AND site_prefix_id IS NOT NULL;
	`)
	return err
}

func init() {
	Migrations.MustRegister(ipBlockManagedUpMigration, nil)
}
