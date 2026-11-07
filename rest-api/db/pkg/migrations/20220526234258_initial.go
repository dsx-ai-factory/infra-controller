// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
)

func init() {
	Migrations.MustRegister(func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [up migration] ")

		// Start transactions
		tx, terr := db.BeginTx(ctx, &sql.TxOptions{})
		if terr != nil {
			handlePanic(terr, "failed to begin transaction")
		}

		// Create Infrastructure Provider table
		_, err := tx.NewCreateTable().Model((*model.InfrastructureProvider)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create Tenant table
		_, err = tx.NewCreateTable().Model((*model.Tenant)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create User table
		_, err = tx.NewCreateTable().Model((*model.User)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create Tenant Account table
		_, err = tx.NewCreateTable().Model((*model.TenantAccount)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create Site table
		_, err = tx.NewCreateTable().Model((*model.Site)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create Security Group table
		_, err = tx.NewCreateTable().Model((*model.NetworkSecurityGroup)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create NVLink Logical Partition table
		_, err = tx.NewCreateTable().Model((*model.NVLinkLogicalPartition)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create VPC table
		_, err = tx.NewCreateTable().Model((*model.Vpc)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create IPBlock table
		_, err = tx.NewCreateTable().Model((*model.IPBlock)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create Allocation table
		_, err = tx.NewCreateTable().Model((*model.Allocation)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create AllocationConstraint table
		_, err = tx.NewCreateTable().Model((*model.AllocationConstraint)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create OperatingSystem table
		_, err = tx.NewCreateTable().Model((*model.OperatingSystem)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create Domain table
		_, err = tx.NewCreateTable().Model((*model.Domain)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create Subnet table
		_, err = tx.NewCreateTable().Model((*model.Subnet)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create InstanceType table
		_, err = tx.NewCreateTable().Model((*model.InstanceType)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create Machine table
		_, err = tx.NewCreateTable().Model((*model.Machine)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create MachineCapability table
		_, err = tx.NewCreateTable().Model((*model.MachineCapability)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create MachineInterface table
		_, err = tx.NewCreateTable().Model((*model.MachineInterface)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create MachineInstanceType table
		_, err = tx.NewCreateTable().Model((*model.MachineInstanceType)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create Instance table
		_, err = tx.NewCreateTable().Model((*model.Instance)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create Interface table
		_, err = tx.NewCreateTable().Model((*model.Interface)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create StatusDetail table
		_, err = tx.NewCreateTable().Model((*model.StatusDetail)(nil)).Exec(ctx)
		handleError(tx, err)

		// Create the ipam table for cloud-ipam
		ipamStorage := cipam.NewBunStorage(db, &tx)
		err = ipamStorage.ApplyDbSchema()
		handleError(tx, err)

		terr = tx.Commit()
		if terr != nil {
			handlePanic(terr, "failed to commit transaction")
		}

		return nil
	}, func(ctx context.Context, db *bun.DB) error {
		fmt.Print(" [down migration] ")
		return nil
	})
}
