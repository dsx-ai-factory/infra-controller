// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Fresh REST installs create this table from the current Bun model.
		_, err := db.ExecContext(ctx, "ALTER TABLE expected_rack ADD COLUMN IF NOT EXISTS rack_group_id varchar")
		return err
	}, func(ctx context.Context, db *bun.DB) error {
		// Keep the additive column so a rollback does not discard group identity.
		return nil
	})
}
