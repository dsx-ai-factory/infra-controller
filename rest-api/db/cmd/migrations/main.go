// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"strconv"
	"strings"

	"github.com/uptrace/bun/extra/bundebug"
	"github.com/uptrace/bun/migrate"

	"github.com/urfave/cli/v2"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/migrations"
)

func main() {
	// Get DB params
	dbHost := os.Getenv("PGHOST")
	if dbHost == "" {
		log.Print("PGHOST is not set, using `localhost` as default")
		dbHost = "localhost"
	}

	dbPortStr := os.Getenv("PGPORT")
	if dbPortStr == "" {
		log.Print("PGPORT is not set, using `5432` as default")
		dbPortStr = "5432"
	}

	dbPort, err := strconv.Atoi(dbPortStr)
	if err != nil {
		log.Fatal(fmt.Errorf("failed to convert PGPORT value: %w", err))
	}

	dbName := os.Getenv("PGDATABASE")
	if dbName == "" {
		log.Print("PGDATABASE is not set, using `nico` as default")
		dbName = "nico"
	}

	dbUser := os.Getenv("PGUSER")
	if dbUser == "" {
		log.Fatal("PGUSER is not set, unable to continue")
	}

	dbPassword := os.Getenv("PGPASSWORD")
	dbPasswordPath := os.Getenv("PGPASSWORD_PATH")

	if dbPassword != "" {
		// Use the password from the environment variable
		log.Print("Using password from PGPASSWORD environment variable")
	} else if dbPasswordPath != "" {
		// Use the password from the file
		log.Print("Using password from PGPASSWORD_PATH environment variable")
		dbPasswordBytes, serr := os.ReadFile(dbPasswordPath)
		if serr != nil {
			log.Fatal(fmt.Errorf("failed to read PGPASSWORD_PATH file: %w", serr))
		}
		dbPassword = string(dbPasswordBytes)
	} else {
		log.Fatal("neither PGPASSWORD is not set, and PGPASSWORD_PATH is not set, unable to continue")
	}

	dbCACertPath := os.Getenv("PGSSLROOTCERT")
	if dbCACertPath != "" {
		// Use the CA certificate from the environment variable
		log.Print("Using PGSSLROOTCERT from environment variable")
	}

	dbSession, err := db.NewSession(context.Background(), dbHost, dbPort, dbName, dbUser, dbPassword, dbCACertPath)
	if err != nil {
		panic(err)
	}

	db := dbSession.DB
	db.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))

	migrator := migrate.NewMigrator(db, migrations.Migrations, migrate.WithMarkAppliedOnSuccess(true))

	app := &cli.App{
		Name: "nico",

		Commands: []*cli.Command{
			newDBCommand(migrator),
		},
	}
	if err := app.Run(os.Args); err != nil {
		log.Fatal(err)
	}
}

func newDBCommand(migrator *migrate.Migrator) *cli.Command {
	return &cli.Command{
		Name:  "db",
		Usage: "database migrations",
		Subcommands: []*cli.Command{
			{
				Name:  "init",
				Usage: "create migration tables",
				Action: func(c *cli.Context) error {
					err := migrator.Init(c.Context)
					if err != nil {
						return err
					}
					fmt.Printf("Initialized migration tables\n")
					return nil
				},
			},
			{
				Name:  "migrate",
				Usage: "migrate database",
				Action: func(c *cli.Context) error {
					group, err := migrator.Migrate(c.Context)
					if err != nil {
						return err
					}
					if group.IsZero() {
						fmt.Printf("There are no new migrations to run (database is up to date)\n")
						return nil
					}
					fmt.Printf("Migrated to: %s\n", group)
					return nil
				},
			},
			{
				Name:  "init_migrate",
				Usage: "initialize & execute migrations",
				Action: func(c *cli.Context) error {
					// Initialize migration tables
					err := migrator.Init(c.Context)
					if err != nil {
						return err
					}
					fmt.Printf("Initialized migration tables\n")

					// Execute migrations
					group, err := migrator.Migrate(c.Context)
					if err != nil {
						return err
					}
					if group.IsZero() {
						fmt.Printf("There are no new migrations to run (database is up to date)\n")
						return nil
					}
					fmt.Printf("Migrated to: %s\n", group)
					return nil
				},
			},
			{
				Name:  "rollback",
				Usage: "rollback the last migration group",
				Action: func(c *cli.Context) error {
					group, err := migrator.Rollback(c.Context)
					if err != nil {
						return err
					}
					if group.IsZero() {
						fmt.Printf("There are no groups to roll back\n")
						return nil
					}
					fmt.Printf("Rolled back: %s\n", group)
					return nil
				},
			},
			{
				Name:  "lock",
				Usage: "lock migrations",
				Action: func(c *cli.Context) error {
					return migrator.Lock(c.Context)
				},
			},
			{
				Name:  "unlock",
				Usage: "unlock migrations",
				Action: func(c *cli.Context) error {
					return migrator.Unlock(c.Context)
				},
			},
			{
				Name:  "create_go",
				Usage: "create Go migration",
				Action: func(c *cli.Context) error {
					name := strings.Join(c.Args().Slice(), "_")
					mf, err := migrator.CreateGoMigration(c.Context, name)
					if err != nil {
						return err
					}
					fmt.Printf("Created migration: %s (%s)\n", mf.Name, mf.Path)
					return nil
				},
			},
			{
				Name:  "create_sql",
				Usage: "create up and down SQL migrations",
				Action: func(c *cli.Context) error {
					name := strings.Join(c.Args().Slice(), "_")
					files, err := migrator.CreateSQLMigrations(c.Context, name)
					if err != nil {
						return err
					}

					for _, mf := range files {
						fmt.Printf("Created migration: %s (%s)\n", mf.Name, mf.Path)
					}

					return nil
				},
			},
			{
				Name:  "status",
				Usage: "print migrations status",
				Action: func(c *cli.Context) error {
					ms, err := migrator.MigrationsWithStatus(c.Context)
					if err != nil {
						return err
					}
					fmt.Printf("Migrations: %s\n", ms)
					fmt.Printf("Unapplied migrations: %s\n", ms.Unapplied())
					fmt.Printf("Last migration group: %s\n", ms.LastGroup())
					return nil
				},
			},
			{
				Name:  "mark_applied",
				Usage: "mark migrations as applied without actually running them",
				Action: func(c *cli.Context) error {
					group, err := migrator.Migrate(c.Context, migrate.WithNopMigration())
					if err != nil {
						return err
					}
					if group.IsZero() {
						fmt.Printf("There are no new migrations to mark as applied\n")
						return nil
					}
					fmt.Printf("Marked as applied: %s\n", group)
					return nil
				},
			},
		},
	}
}
