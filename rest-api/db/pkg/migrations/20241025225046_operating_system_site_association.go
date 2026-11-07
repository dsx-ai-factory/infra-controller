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
			handlePanic(terr, "failed to begin transaction")
		}

		// Create OperatingSystemSiteAssociation table
		_, err := tx.NewCreateTable().Model((*model.OperatingSystemSiteAssociation)(nil)).Exec(ctx)
		handleError(tx, err)

		// Drop if the operating_system_id_idx index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS operating_system_id_idx")
		handleError(tx, err)

		// Add operating_system_id_idx index for operating_system_site_association table
		_, err = tx.Exec("CREATE INDEX operating_system_id_idx ON public.operating_system_site_association(operating_system_id)")
		handleError(tx, err)

		// Drop if the site_id_idx index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS site_id_idx")
		handleError(tx, err)

		// Add site_id_idx index for operating_system_site_association table
		_, err = tx.Exec("CREATE INDEX site_id_idx ON public.operating_system_site_association(site_id)")
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
