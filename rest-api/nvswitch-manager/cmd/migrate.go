// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/db/migrations"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/db/postgres"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
)

var (
	migrateDBHost         string
	migrateDBPort         int
	migrateDBName         string
	migrateDBUser         string
	migrateDBUserPassword string
	migrateDBCertificate  string
	rollBack              string

	// migrateCmd represents the migrate command
	migrateCmd = &cobra.Command{
		Use:   "migrate",
		Short: "Run the db migration",
		Long:  `Run the db migration for NV-Switch Manager`,
		Run: func(cmd *cobra.Command, args []string) {
			doMigration()
		},
	}
)

func init() {
	rootCmd.AddCommand(migrateCmd)

	migrateCmd.Flags().StringVarP(&migrateDBHost, "host", "s", defaultDbHostName, "host")                                                                                                                    //nolint
	migrateCmd.Flags().IntVarP(&migrateDBPort, "port", "p", defaultDbPort, "port")                                                                                                                           //nolint
	migrateCmd.Flags().StringVarP(&migrateDBName, "dbname", "d", defaultDbName, "database name")                                                                                                             //nolint
	migrateCmd.Flags().StringVarP(&migrateDBUser, "user", "u", defaultDbUser, "user")                                                                                                                        //nolint
	migrateCmd.Flags().StringVarP(&migrateDBUserPassword, "password", "w", defaultDbPassword, "password")                                                                                                    //nolint
	migrateCmd.Flags().StringVarP(&migrateDBCertificate, "certificate", "c", "", "certificate path")                                                                                                         //nolint
	migrateCmd.Flags().StringVarP(&rollBack, "rollback", "r", "", "Roll back the schema to the way it was at the specified time. This is the application time, not from the ID. Format 2006-01-02T15:04:05") //nolint
}

func doMigration() {
	ctx := context.Background()

	dbConf := db.Config{
		Host:              migrateDBHost,
		Port:              migrateDBPort,
		DBName:            migrateDBName,
		CACertificatePath: migrateDBCertificate,
	}

	dbConf.Credential.Update(&migrateDBUser, &migrateDBUserPassword)

	db, err := postgres.New(ctx, dbConf)
	if err != nil {
		log.Fatalf("failed to connect to DB: %v", err)
	}

	if rollBack != "" {
		rollbackTime, err := time.Parse("2006-01-02T15:04:05", rollBack)
		if err != nil {
			log.Fatal("Bad rollback time format. Expected format: 2006-01-02T15:04:05")
		}
		if err := migrations.Rollback(ctx, db, rollbackTime); err != nil {
			log.Fatalf("Failed to roll back migrations: %v", err)
		}
	} else {
		if err := migrations.Migrate(ctx, db); err != nil {
			log.Fatalf("Failed to run migrations: %v", err)
		}
	}
}
