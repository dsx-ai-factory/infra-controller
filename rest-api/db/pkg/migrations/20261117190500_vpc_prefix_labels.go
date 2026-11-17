// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		_, err := db.ExecContext(ctx, `
			ALTER TABLE vpc_prefix ADD COLUMN IF NOT EXISTS labels JSONB;
			UPDATE vpc_prefix SET labels = '{}'::jsonb WHERE labels IS NULL;
			ALTER TABLE vpc_prefix ALTER COLUMN labels SET DEFAULT '{}'::jsonb;
			ALTER TABLE vpc_prefix ALTER COLUMN labels SET NOT NULL;
		`)
		return err
	}, nil)
}
