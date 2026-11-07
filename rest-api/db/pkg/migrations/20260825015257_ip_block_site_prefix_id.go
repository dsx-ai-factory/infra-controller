// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"fmt"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Fresh databases create `ip_block` with this column. Bun can also replay
		// the callback after it commits but before the migration is recorded.
		_, err := db.ExecContext(ctx, `ALTER TABLE ip_block ADD COLUMN IF NOT EXISTS site_prefix_id UUID`)
		if err != nil {
			return err
		}

		fmt.Print(" [up migration] Ensured 'site_prefix_id' exists on 'ip_block' table. ")
		return nil
	}, func(_ context.Context, _ *bun.DB) error {
		// Older binaries ignore this additive column, so leave stored Core IDs in place.
		fmt.Print(" [down migration] No action taken")
		return nil
	})
}
