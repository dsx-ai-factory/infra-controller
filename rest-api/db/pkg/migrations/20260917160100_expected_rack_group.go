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
		tx, err := db.BeginTx(ctx, &sql.TxOptions{})
		if err != nil {
			handlePanic(err, "failed to begin transaction")
		}

		_, err = tx.NewCreateTable().Model((*model.ExpectedRackGroup)(nil)).IfNotExists().Exec(ctx)
		handleError(tx, err)
		_, err = tx.Exec("ALTER TABLE expected_rack_group ADD CONSTRAINT expected_rack_group_group_id_site_id_key UNIQUE (rack_group_id, site_id) DEFERRABLE INITIALLY DEFERRED")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX expected_rack_group_site_id_idx ON expected_rack_group(site_id)")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX expected_rack_group_created_idx ON expected_rack_group(created)")
		handleError(tx, err)

		if err := tx.Commit(); err != nil {
			handlePanic(err, "failed to commit transaction")
		}
		fmt.Print(" [up migration] Created 'expected_rack_group' table and indices successfully. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] No action taken")
		return nil
	})
}
