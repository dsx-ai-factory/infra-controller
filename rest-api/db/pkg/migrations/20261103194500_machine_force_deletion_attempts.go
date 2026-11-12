// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Existing retained markers carry no proof that Core deletion completed.
		// Fresh REST bootstrap creates tables from current models, including these columns.
		_, err := db.ExecContext(ctx, `ALTER TABLE machine
			ADD COLUMN IF NOT EXISTS is_force_deletion_complete BOOLEAN NOT NULL DEFAULT false,
			ADD COLUMN IF NOT EXISTS force_deletion_workflow_id TEXT`)
		return err
	}, func(ctx context.Context, db *bun.DB) error {
		return nil
	})
}
