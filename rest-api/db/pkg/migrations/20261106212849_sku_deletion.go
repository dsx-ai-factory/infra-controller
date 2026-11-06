// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// SKU bootstrap uses the current model, which already includes this column.
		_, err := db.ExecContext(ctx, `ALTER TABLE sku ADD COLUMN IF NOT EXISTS deletion JSONB`)
		return err
	}, func(ctx context.Context, db *bun.DB) error { return nil })
}
