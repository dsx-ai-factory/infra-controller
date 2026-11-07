// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Start transactions
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			fmt.Println("failed to begin transaction: ", terr)
			return terr
		}

		// Add AuxID column to User table
		_, err := tx.NewAddColumn().Model((*model.User)(nil)).IfNotExists().ColumnExpr("auxiliary_id TEXT").Exec(ctx)
		if err != nil {
			fmt.Println("failed to add column: ", err)
			return err
		}

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS user_auxiliary_id_indx")
		handleError(tx, err)

		_, err = tx.Exec("CREATE INDEX user_auxiliary_id_indx ON public.user(auxiliary_id)")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}
		fmt.Print(" [up migration] Added 'auxiliary_id' column to 'user' table successfully and created index 'user_auxiliary_id_indx'")

		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		// Start transactions
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			fmt.Println("failed to begin transaction: ", terr)
			return terr
		}

		// Remove AuxID column from User table
		_, err := tx.NewDropColumn().Model((*model.User)(nil)).Column("auxiliary_id").Exec(ctx)
		if err != nil {
			fmt.Println("failed to drop column: ", err)
			return err
		}

		terr = tx.Commit()
		if terr != nil {
			fmt.Println("failed to commit transaction: ", terr)
			return terr
		}

		fmt.Print(" [down migration] Removed 'aux_id' column from 'user' table successfully.")
		return nil
	})
}
