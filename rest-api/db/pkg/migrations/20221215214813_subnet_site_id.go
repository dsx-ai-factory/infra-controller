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

func subnetSiteIDUpMigration(ctx context.Context, db *bun.DB) error {
	// Start transactions
	tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
	if terr != nil {
		handlePanic(terr, "failed to begin transaction")
	}

	// Add site_id column to subnet table
	_, err := tx.NewAddColumn().Model((*model.Subnet)(nil)).IfNotExists().ColumnExpr("site_id UUID NULL").Exec(ctx)
	handleError(tx, err)

	// Add site_id foreign key constraint for subnet
	// Drop if one exists (won't occur/harmless in dev/stage/prod but helps with test)
	_, err = tx.Exec("ALTER TABLE subnet DROP CONSTRAINT IF EXISTS subnet_site_id_fkey")
	handleError(tx, err)

	// Add site_id foreign key constraint
	_, err = tx.Exec("ALTER TABLE subnet ADD CONSTRAINT subnet_site_id_fkey FOREIGN KEY (site_id) REFERENCES public.site(id)")
	handleError(tx, err)

	// Insert infrastructure provider info
	subnets := []model.Subnet{}
	err = tx.NewSelect().Model(&subnets).Relation(model.VpcRelationName).WhereAllWithDeleted().Scan(ctx)
	handleError(tx, err)

	// Update Subnets with Site ID from VPCs
	updatedFields := []string{"site_id"}
	for _, subnet := range subnets {
		if subnet.Vpc != nil {
			curSubnet := subnet
			curSubnet.SiteID = curSubnet.Vpc.SiteID

			// Update infrastructure_provider_id
			_, err = tx.NewUpdate().Model(&curSubnet).Column(updatedFields...).WhereAllWithDeleted().Where("id = ?", subnet.ID).Exec(ctx)
			handleError(tx, err)
		}
	}

	// Update infrastructure_provider_id column as not null to vpc table
	_, err = tx.Exec("ALTER TABLE subnet ALTER COLUMN site_id SET NOT NULL")
	handleError(tx, err)

	terr = tx.Commit()
	if terr != nil {
		handlePanic(terr, "failed to commit transaction")
	}

	fmt.Print(" [up migration] ")
	return nil
}

func init() {
	Migrations.MustRegister(subnetSiteIDUpMigration, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
