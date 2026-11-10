// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Ordinary and transiently missing Machines have not accepted forced deletion.
		_, err := db.ExecContext(ctx, "ALTER TABLE machine ADD COLUMN IF NOT EXISTS is_force_deletion_requested BOOLEAN NOT NULL DEFAULT false")
		return err
	}, func(ctx context.Context, db *bun.DB) error {
		return nil
	})
}
