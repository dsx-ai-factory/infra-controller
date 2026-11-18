// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func expectedRackImmediateUniqueUpMigration(ctx context.Context, db *bun.DB) error {
	// Inventory can see a Core rack before its REST create transaction commits.
	// Check uniqueness before inserting the competing index entry so the API
	// can persist Core's derived profile and commit without a deferred-check deadlock.
	_, err := db.ExecContext(ctx, `ALTER TABLE expected_rack
		DROP CONSTRAINT expected_rack_rack_id_site_id_key,
		ADD CONSTRAINT expected_rack_rack_id_site_id_key UNIQUE (rack_id, site_id)`)
	return err
}

func init() {
	Migrations.MustRegister(expectedRackImmediateUniqueUpMigration, func(ctx context.Context, db *bun.DB) error {
		_, err := db.ExecContext(ctx, `ALTER TABLE expected_rack
			DROP CONSTRAINT expected_rack_rack_id_site_id_key,
			ADD CONSTRAINT expected_rack_rack_id_site_id_key UNIQUE (rack_id, site_id) DEFERRABLE INITIALLY DEFERRED`)
		return err
	})
}
