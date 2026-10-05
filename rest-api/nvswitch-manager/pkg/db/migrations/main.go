// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"crypto/md5"
	"embed"
	"encoding/hex"
	"fmt"
	"io"
	"io/fs"
	"sort"
	"strings"
	"time"

	"github.com/dsx-ai-factory/infra-controller/rest-api/nvswitch-manager/pkg/db/postgres"

	log "github.com/sirupsen/logrus"
	"github.com/uptrace/bun"
)

//go:embed *.sql
var sqlMigrations embed.FS

// lockOrCreateMigrationTable will return with the applied migrations table present and locked.  On the very first
// run if we had multiple instances trying this, some may end up having their commit aborted due to conflicts and will restart.
func lockOrCreateMigrationTable(ctx context.Context, tx *bun.Tx) error {
	// We cannot try just locking first - something is rolling back transactions on the first seen error
	_, err := tx.ExecContext(ctx, `CREATE TABLE IF NOT EXISTS migrations (
					id TEXT NOT NULL PRIMARY KEY,
					name TEXT NOT NULL,
					hash TEXT NOT NULL,
					applied_date TIMESTAMP NOT NULL DEFAULT NOW()
					)`)
	if err != nil {
		return err
	}

	_, err = tx.ExecContext(ctx, "LOCK TABLE migrations")
	if err != nil {
		return err
	}

	return nil
}

type appliedMigration struct {
	id      string
	hash    string
	applied time.Time
}

// appliedMigrations retrieves the already applied migrations
func appliedMigrations(ctx context.Context, tx *bun.Tx) (applied map[string]appliedMigration, err error) {
	applied = make(map[string]appliedMigration)

	appliedRows, err := tx.QueryContext(ctx, "SELECT id, hash, applied_date FROM migrations")
	if err != nil {
		return nil, err
	}
	defer appliedRows.Close()

	for appliedRows.Next() {
		var migration appliedMigration

		if err := appliedRows.Scan(&migration.id, &migration.hash, &migration.applied); err != nil {
			return nil, err
		}
		applied[migration.id] = migration
	}

	return applied, nil
}

// parseMigrationFilename expects that the filename be of the form ID_NAME.up.sql or ID_NAME.down.sql, with ID always being a timestamp in practice
func parseMigrationFilename(path string, is_up bool) (id string, name string, ok bool) {
	var pathTrimmed string
	if is_up {
		if !strings.HasSuffix(path, ".up.sql") {
			return "", "", false
		}
		pathTrimmed = strings.TrimSuffix(path, ".up.sql")
	} else {
		if !strings.HasSuffix(path, ".down.sql") {
			return "", "", false
		}
		pathTrimmed = strings.TrimSuffix(path, ".down.sql")
	}

	split := strings.Split(pathTrimmed, "_")
	if len(split) != 2 {
		// Malformed filename (should be exactly ID_NAME), skip this file
		return "", "", false
	}

	return split[0], split[1], true
}

func stringHash(contents []byte) string {
	hash := md5.Sum([]byte(contents))
	return hex.EncodeToString(hash[:])
}

func hashMatch(contents []byte, oldHash string) bool {
	return stringHash(contents) == oldHash
}

// applyMigration will apply an individual migration to the database
func applyMigration(ctx context.Context, tx *bun.Tx, id string, name string, contents []byte, is_rollback bool) (err error) {
	if is_rollback {
		log.Infof("Rolling back migration %s (%s)", name, id)
	} else {
		log.Infof("Applying new migration %s (%s)", name, id)
	}

	// Optionally allow splitting up the SQL to make the location of an error more obvious
	splitContents := strings.Split(string(contents), "-- SECTION")
	for _, cur := range splitContents {
		_, err := tx.ExecContext(ctx, cur)
		if err != nil {
			return fmt.Errorf("Migration for %s failed: %v       Command: %s", id, err, cur)
		}
	}

	// All sections succeeded, mark success
	if is_rollback {
		_, err = tx.ExecContext(ctx, "DELETE FROM migrations WHERE id = ?0", id)
	} else {
		_, err = tx.ExecContext(ctx, "INSERT INTO migrations (id, name, hash) VALUES (?0, ?1, ?2)", id, name, stringHash(contents))
	}
	return err
}

func alternatePresent(path string) bool {
	var altpath string
	if strings.HasSuffix(path, ".up.sql") {
		altpath = strings.TrimSuffix(path, ".up.sql") + ".down.sql"
	} else {
		altpath = strings.TrimSuffix(path, ".down.sql") + ".up.sql"
	}

	if file, err := sqlMigrations.Open(altpath); err == nil {
		file.Close()
		return true
	}

	return false
}

// Migrate ensures that the database contains all currently known migrations
func Migrate(ctx context.Context, db *postgres.Postgres) error {
	return migrateInternal(ctx, db, nil)
}

// Rollback will roll back migrations that have been applied since the given time
func Rollback(ctx context.Context, db *postgres.Postgres, rollbackTime time.Time) error {
	return migrateInternal(ctx, db, &rollbackTime)
}

type pendingMigration struct {
	id       string
	name     string
	contents []byte
}

// migrateInternal migrates either up or down, but in an inconvient calling method
func migrateInternal(ctx context.Context, db *postgres.Postgres, rollbackTime *time.Time) (errFinal error) {
	return db.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		if err := lockOrCreateMigrationTable(ctx, &tx); err != nil {
			return err
		}

		appliedMigrations, err := appliedMigrations(ctx, &tx)
		if err != nil {
			return err
		}

		isRollback := rollbackTime != nil

		// Collect all migrations that need to be applied/rolled back
		var pending []pendingMigration
		if err := fs.WalkDir(sqlMigrations, ".", func(path string, d fs.DirEntry, err error) error {
			if err != nil {
				return fmt.Errorf("error accessing migration file %s: %w", path, err)
			}

			id, name, ok := parseMigrationFilename(path, !isRollback)
			if !ok {
				return nil
			}
			if !alternatePresent(path) {
				return fmt.Errorf("Migration file %s does not have a matching down/up migration", path)
			}
			file, err := sqlMigrations.Open(path)
			if err != nil {
				return err
			}
			defer file.Close()
			contents, err := io.ReadAll(file)
			if err != nil {
				return err
			}

			migration, applied := appliedMigrations[id]
			if isRollback {
				if applied && rollbackTime.Before(migration.applied) {
					pending = append(pending, pendingMigration{id: id, name: name, contents: contents})
				}
			} else {
				if applied {
					if !hashMatch(contents, migration.hash) && !strings.Contains(string(contents), "Allow hash changing") {
						return fmt.Errorf("Hash for migration %s (%s) does not match already applied migration.  Something inappropriately altered the migration.  Aborting.", name, id)
					}
				} else {
					pending = append(pending, pendingMigration{id: id, name: name, contents: contents})
				}
			}
			return nil
		}); err != nil {
			return err
		}

		if len(pending) == 0 {
			log.Info("Database schema up to date, no migrations applied")
			return nil
		}

		// Rollbacks run newest-first; forward migrations run oldest-first (already
		// in ascending order from fs.WalkDir's alphabetical traversal).
		if isRollback {
			sort.Slice(pending, func(i, j int) bool {
				return pending[i].id > pending[j].id
			})
		}

		for _, m := range pending {
			if err := applyMigration(ctx, &tx, m.id, m.name, m.contents, isRollback); err != nil {
				return err
			}
		}

		return nil
	})
}
