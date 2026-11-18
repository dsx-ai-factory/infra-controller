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
		// Fresh installs create this column from the current Interface model.
		// Existing rows receive their prefixes from the next inventory report.
		_, err := db.ExecContext(ctx, `ALTER TABLE "interface" ADD COLUMN IF NOT EXISTS ip_prefixes TEXT[]`)
		if err != nil {
			return err
		}

		fmt.Print(" [up migration] Ensured 'ip_prefixes' exists on 'interface' table. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		_, err := db.ExecContext(ctx, `ALTER TABLE "interface" DROP COLUMN ip_prefixes`)
		return err
	})
}
