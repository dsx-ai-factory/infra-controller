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

		// create audit entry table
		_, err := tx.NewCreateTable().Model((*model.AuditEntry)(nil)).Exec(ctx)
		handleError(tx, err)

		// add indexes
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS audit_entry_endpoint_idx ON public.audit_entry(endpoint)")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS audit_entry_method_idx ON public.audit_entry(method)")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS audit_entry_status_code_idx ON public.audit_entry(status_code)")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS audit_entry_client_ip_idx ON public.audit_entry(client_ip)")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS audit_entry_user_id_idx ON public.audit_entry(user_id)")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS audit_entry_org_name_idx ON public.audit_entry(org_name)")
		handleError(tx, err)
		_, err = tx.Exec("CREATE INDEX IF NOT EXISTS audit_entry_timestamp_idx ON public.audit_entry(timestamp)")
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
