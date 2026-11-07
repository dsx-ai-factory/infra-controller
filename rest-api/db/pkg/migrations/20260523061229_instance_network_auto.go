// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// network_auto carries the request-time signal that NICo should
		// auto-resolve the instance's network interfaces from the host's
		// HostInband segments. Default FALSE preserves the historical
		// "explicit interfaces required" contract for existing rows.
		_, err := tx.ExecContext(ctx, `ALTER TABLE "instance" ADD COLUMN IF NOT EXISTS network_auto BOOLEAN NOT NULL DEFAULT FALSE`)
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added 'network_auto' column to 'instance' table successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		if _, err := db.ExecContext(ctx, `ALTER TABLE "instance" DROP COLUMN IF EXISTS network_auto`); err != nil {
			return err
		}
		fmt.Print(" [down migration] Dropped 'network_auto' column from 'instance' table successfully. ")
		return nil
	})
}
