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
		// Machine placement filters use JSONB containment (labels @> filter).
		// jsonb_path_ops keeps the index smaller while supporting that operator.
		_, err := db.ExecContext(ctx, `
			CREATE INDEX IF NOT EXISTS machine_labels_gin_idx
			ON public.machine USING GIN (labels jsonb_path_ops)
		`)
		if err != nil {
			return err
		}

		fmt.Print(" [up migration] Added GIN index for Machine label containment queries. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		_, err := db.ExecContext(ctx, "DROP INDEX IF EXISTS public.machine_labels_gin_idx")
		if err != nil {
			return err
		}

		fmt.Print(" [down migration] Dropped Machine labels GIN index. ")
		return nil
	})
}
