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

func vpcProviderIDUpMigration(ctx context.Context, db *bun.DB) error {
	// Start transactions
	tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
	if terr != nil {
		handlePanic(terr, "failed to begin transaction")
	}

	// Add infrastructure_provider_id column to vpc table
	_, err := tx.NewAddColumn().Model((*model.Vpc)(nil)).IfNotExists().ColumnExpr("infrastructure_provider_id UUID NULL").Exec(ctx)
	handleError(tx, err)

	// Add infrastructure provider foreign key for VPC
	// Drop if one exists (won't occur/harmless in dev/stage/prod but helps with test)
	_, err = tx.Exec("ALTER TABLE vpc DROP CONSTRAINT IF EXISTS vpc_infrastructure_provider_id_fkey")
	handleError(tx, err)

	// Add infrastructure provider foreign key
	_, err = tx.Exec("ALTER TABLE vpc ADD CONSTRAINT vpc_infrastructure_provider_id_fkey FOREIGN KEY (infrastructure_provider_id) REFERENCES public.infrastructure_provider(id)")
	handleError(tx, err)

	// Insert infrastructure provider info
	vpcs := []model.Vpc{}
	err = tx.NewSelect().Model(&vpcs).Relation(model.SiteRelationName).WhereAllWithDeleted().Scan(ctx)
	handleError(tx, err)

	// Prepare vpcs with infrastructure provider from site's infrastructure provider
	updatedFields := []string{"infrastructure_provider_id"}
	for _, vpc := range vpcs {
		if vpc.Site != nil {
			curVpc := vpc
			curVpc.InfrastructureProviderID = curVpc.Site.InfrastructureProviderID

			// Update infrastructure_provider_id
			_, err = tx.NewUpdate().Model(&curVpc).Column(updatedFields...).WhereAllWithDeleted().Where("id = ?", curVpc.ID).Exec(ctx)
			handleError(tx, err)
		}
	}

	// Update infrastructure_provider_id column as not null to vpc table
	_, err = tx.Exec("ALTER TABLE vpc ALTER COLUMN infrastructure_provider_id SET NOT NULL")
	handleError(tx, err)

	terr = tx.Commit()
	if terr != nil {
		handlePanic(terr, "failed to commit transaction")
	}

	fmt.Print(" [up migration] ")
	return nil
}

func init() {
	Migrations.MustRegister(vpcProviderIDUpMigration, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
