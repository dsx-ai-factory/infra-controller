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

		// Create SpectrumXPartition table
		_, err := tx.NewCreateTable().Model((*model.SpectrumXPartition)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create SpectrumXAttachment table. Declared after the Partition table
		// because it carries a foreign key to it.
		_, err = tx.NewCreateTable().Model((*model.SpectrumXAttachment)(nil)).Exec(ctx)
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS spectrumx_partition_gin_idx")
		handleError(tx, err)

		// Add GIN index for spectrumx_partition table
		_, err = tx.Exec("CREATE INDEX spectrumx_partition_gin_idx ON public.spectrumx_partition USING GIN (name gin_trgm_ops, description gin_trgm_ops, status gin_trgm_ops)")
		handleError(tx, err)

		// Drop if the index exists (won't occur/harmless in dev/stage/prod but helps with test)
		_, err = tx.Exec("DROP INDEX IF EXISTS spectrumx_attachment_gin_idx")
		handleError(tx, err)

		// Add GIN index for spectrumx_attachment table
		_, err = tx.Exec("CREATE INDEX spectrumx_attachment_gin_idx ON public.spectrumx_attachment USING GIN (device gin_trgm_ops, attachment_type gin_trgm_ops, mac_address gin_trgm_ops, ip_address gin_trgm_ops, status gin_trgm_ops)")
		handleError(tx, err)

		// Indexes backing the default order-by and the common filters, matching
		// the set the InfiniBand Partition and Interface tables carry.
		indexes := []struct {
			index  string
			table  string
			column string
		}{
			{index: "spectrumx_partition_created_idx", table: "public.spectrumx_partition", column: "created"},
			{index: "spectrumx_partition_site_id_idx", table: "public.spectrumx_partition", column: "site_id"},
			{index: "spectrumx_partition_tenant_id_idx", table: "public.spectrumx_partition", column: "tenant_id"},
			{index: "spectrumx_attachment_created_idx", table: "public.spectrumx_attachment", column: "created"},
			{index: "spectrumx_attachment_site_id_idx", table: "public.spectrumx_attachment", column: "site_id"},
			{index: "spectrumx_attachment_instance_id_idx", table: "public.spectrumx_attachment", column: "instance_id"},
			{index: "spectrumx_attachment_partition_id_idx", table: "public.spectrumx_attachment", column: "spectrumx_partition_id"},
		}

		for _, idx := range indexes {
			_, err = tx.Exec(fmt.Sprintf("DROP INDEX IF EXISTS %s", idx.index))
			handleError(tx, err)

			_, err = tx.Exec(fmt.Sprintf("CREATE INDEX %s ON %s (%s)", idx.index, idx.table, idx.column))
			handleError(tx, err)
		}

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		fmt.Print(" [up migration] Added SpectrumX Partition and Attachment tables. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
