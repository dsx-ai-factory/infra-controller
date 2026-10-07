// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Fresh REST installs create this column from the current Bun model.
		_, err := db.ExecContext(ctx, "ALTER TABLE expected_rack_group ADD COLUMN IF NOT EXISTS protocol varchar")
		return err
	}, func(ctx context.Context, db *bun.DB) error {
		// Preserve legacy declarations if the application is rolled back.
		return nil
	})
}
