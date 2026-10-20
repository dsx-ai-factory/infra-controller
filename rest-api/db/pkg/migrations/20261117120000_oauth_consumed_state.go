// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/uptrace/bun"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		// Record redeemed OAuth state digests until expiry so callback states are single-use across replicas.
		return db.RunInTx(ctx, nil, func(ctx context.Context, tx bun.Tx) error {
			_, err := tx.NewCreateTable().Model((*model.OAuthConsumedState)(nil)).IfNotExists().Exec(ctx)
			if err != nil {
				return err
			}
			_, err = tx.NewCreateIndex().
				Model((*model.OAuthConsumedState)(nil)).
				IfNotExists().
				Index("oauth_consumed_state_expires_at_idx").
				Column("expires_at").
				Exec(ctx)
			return err
		})
	}, func(ctx context.Context, db *bun.DB) error {
		_, err := db.NewDropTable().Model((*model.OAuthConsumedState)(nil)).IfExists().Exec(ctx)
		return err
	})
}
