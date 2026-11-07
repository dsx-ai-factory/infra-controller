// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"
)

func instanceAutoNetworkRenameUpMigration(ctx context.Context, db *bun.DB) error {
	// Start transaction
	tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
	if terr != nil {
		handlePanic(terr, "failed to begin transaction")
	}

	// Align the column with the renamed `AutoNetwork` model field. Two
	// starting states are possible:
	//   - Existing deployments carry only `network_auto`            -> rename it.
	//   - Fresh DBs build `instance` from the current model (which
	//     already has `auto_network`); the preceding ADD-COLUMN
	//     migration then adds a redundant `network_auto`            -> drop it.
	res, err := tx.Exec("SELECT column_name FROM information_schema.columns WHERE table_name = 'instance' AND column_name = 'network_auto'")
	handleError(tx, err)
	networkAutoRowsAffected, err := res.RowsAffected()
	handleError(tx, err)
	res, err = tx.Exec("SELECT column_name FROM information_schema.columns WHERE table_name = 'instance' AND column_name = 'auto_network'")
	handleError(tx, err)
	autoNetworkRowsAffected, err := res.RowsAffected()
	handleError(tx, err)

	if networkAutoRowsAffected > 0 && autoNetworkRowsAffected == 0 {
		_, err := tx.Exec("ALTER TABLE instance RENAME COLUMN network_auto TO auto_network")
		handleError(tx, err)
	} else if networkAutoRowsAffected > 0 && autoNetworkRowsAffected > 0 {
		_, err := tx.Exec("ALTER TABLE instance DROP COLUMN network_auto")
		handleError(tx, err)
	} else {
		fmt.Println("network_auto rename to auto_network: Migration skipped. Either the column does not exist or already renamed")
	}

	terr = tx.Commit()
	if terr != nil {
		handlePanic(terr, "failed to commit transaction")
	}

	fmt.Print(" [up migration] ")
	return nil
}

func init() {
	Migrations.MustRegister(instanceAutoNetworkRenameUpMigration, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
