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
		// Label discovery filters on key existence, which requires jsonb_ops.
		_, err := db.ExecContext(ctx, `
			CREATE INDEX IF NOT EXISTS machine_labels_gin_ops_idx
			ON public.machine USING GIN (labels)
		`)
		if err != nil {
			return err
		}
		_, err = db.ExecContext(ctx, "DROP INDEX IF EXISTS public.machine_labels_gin_idx")
		if err != nil {
			return err
		}

		fmt.Print(" [up migration] Replaced the Machine labels GIN index with jsonb_ops. ")
		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		_, err := db.ExecContext(ctx, `
			CREATE INDEX IF NOT EXISTS machine_labels_gin_idx
			ON public.machine USING GIN (labels jsonb_path_ops)
		`)
		if err != nil {
			return err
		}
		_, err = db.ExecContext(ctx, "DROP INDEX IF EXISTS public.machine_labels_gin_ops_idx")
		if err != nil {
			return err
		}

		fmt.Print(" [down migration] Restored the Machine labels jsonb_path_ops GIN index. ")
		return nil
	})
}
