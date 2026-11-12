// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// A rejected request must not delete an unrelated Core VPC with the same ID.
		_, err := db.ExecContext(ctx, "ALTER TABLE vpc ADD COLUMN IF NOT EXISTS creation_rejected_at TIMESTAMPTZ NULL")
		return err
	}, func(context.Context, *bun.DB) error {
		return nil
	})
}
