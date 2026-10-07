// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"crypto/md5"
	"embed"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"regexp"
	"sort"
	"strings"
	"time"

	"database/sql"

	"github.com/rs/zerolog/log"
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

	split := strings.SplitN(pathTrimmed, "_", 2)
	if len(split) != 2 {
		log.Fatal().Msgf("Invalid migration filename %q, expected format: TIMESTAMP_NAME.up.sql or TIMESTAMP_NAME.down.sql", path)
	}

	if !regexp.MustCompile(`^[a-z][a-z0-9_]*$`).MatchString(split[1]) {
		log.Fatal().Msgf("Invalid migration name %q in %q, must match [a-z][a-z0-9_]* (lowercase, start with letter, only alphanumeric and underscores)", split[1], path)
	}

	return split[0], split[1], true
}

func stringHash(contents []byte) string {
	hash := md5.Sum(contents)
	return hex.EncodeToString(hash[:])
}

func hashMatch(contents []byte, oldHash string) bool {
	return stringHash(contents) == oldHash
}

func checkMigrationSignature(contents []byte, oldHash string, options MigrateOptions) (shouldUpdateSignature bool, err error) {
	if hashMatch(contents, oldHash) {
		return false, nil
	}
	if options.IgnoreSignatureChanges {
		return true, nil
	}
	if strings.Contains(string(contents), "Allow hash changing") {
		return false, nil
	}
	return false, errors.New("signature does not match")
}

// applyMigration will apply an individual migration to the database
func applyMigration(ctx context.Context, tx *bun.Tx, id string, name string, contents []byte, is_rollback bool) (err error) {
	if is_rollback {
		log.Info().Msgf("Rolling back migration %s (%s)", name, id)
	} else {
		log.Info().Msgf("Applying new migration %s (%s)", name, id)
	}

	// Optionally allow splitting up the SQL to make the location of an error more obvious
	splitContents := strings.Split(string(contents), "-- SECTION")
	for _, cur := range splitContents {
		_, err := tx.Exec(cur)
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

// MigrateOptions controls optional forward-migration behavior.
type MigrateOptions struct {
	// IgnoreSignatureChanges accepts changed contents for migrations that are
	// already installed. The migration SQL is not run again; only the stored
	// hash is updated.
	IgnoreSignatureChanges bool
}

// MigrateWithDB ensures that the database contains all currently known
// migrations using the supplied options.
func MigrateWithDB(ctx context.Context, db *bun.DB, options MigrateOptions) error {
	return migrateInternalWithDB(ctx, db, nil, options)
}

// RollbackWithDB will roll back migrations that have been applied since the given time.
// Accepts a *bun.DB directly.
func RollbackWithDB(ctx context.Context, db *bun.DB, rollbackTime time.Time) error {
	return migrateInternalWithDB(ctx, db, &rollbackTime, MigrateOptions{})
}

// migration holds information needed to apply a migration or update its
// recorded signature.
type migration struct {
	id   string
	name string
	path string
	hash string
}

// readMigrationContents reads the contents of a migration file
func readMigrationContents(path string) ([]byte, error) {
	file, err := sqlMigrations.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	return io.ReadAll(file)
}

// migrateInternalWithDB migrates either up or down using a *bun.DB
func migrateInternalWithDB(ctx context.Context, db *bun.DB, rollbackTime *time.Time, options MigrateOptions) (errFinal error) {
	return db.RunInTx(ctx, &sql.TxOptions{}, func(ctx context.Context, tx bun.Tx) error { //nolint:exhaustruct,varnamelen,wrapcheck // default options; tx is idiomatic; thin wrapper
		// Lock the migration table manually in case we are running multiple instances that may try to upgrade simultaneously
		if err := lockOrCreateMigrationTable(ctx, &tx); err != nil {
			return err
		}

		// Retrieve the migrations that have already been applied
		appliedMigrations, err := appliedMigrations(ctx, &tx)
		if err != nil {
			return err
		}

		isRollback := rollbackTime != nil
		var pending []migration
		var changedSignatures []migration

		// Collect all migrations that need to be applied
		if err := fs.WalkDir(sqlMigrations, ".", func(path string, d fs.DirEntry, err error) error {
			id, name, ok := parseMigrationFilename(path, !isRollback)
			if !ok {
				return nil
			}
			if !alternatePresent(path) {
				return fmt.Errorf("Migration file %s does not have a matching down/up migration", path)
			}

			installedMigration, alreadyApplied := appliedMigrations[id]
			if isRollback {
				// For rollback: only include migrations that were applied after rollbackTime
				if !alreadyApplied || !rollbackTime.Before(installedMigration.applied) {
					return nil
				}
			} else {
				// For forward migration: only include migrations that haven't been applied yet
				if alreadyApplied {
					// Already applied - verify hash hasn't changed
					contents, err := readMigrationContents(path)
					if err != nil {
						return err
					}

					shouldUpdateSignature, err := checkMigrationSignature(contents, installedMigration.hash, options)
					if err != nil {
						return fmt.Errorf("Hash for migration %s (%s) does not match already applied migration.  Something inappropriately altered the migration.  Aborting.", name, id)
					}

					if shouldUpdateSignature {
						changedSignatures = append(
							changedSignatures,
							migration{
								id:   id,
								name: name,
								hash: stringHash(contents),
							},
						)
					}

					return nil
				}
			}

			pending = append(pending, migration{id: id, name: name, path: path})
			return nil
		}); err != nil {
			return err
		}

		for _, migration := range changedSignatures {
			log.Warn().Msgf("Accepting changed signature for installed migration %s (%s) without re-applying it", migration.name, migration.id)
			if _, err := tx.ExecContext(ctx, "UPDATE migrations SET hash = ?0 WHERE id = ?1", migration.hash, migration.id); err != nil {
				return fmt.Errorf("updating signature for installed migration %s (%s): %w", migration.name, migration.id, err)
			}
		}

		// Sort migrations in the appropriate order
		if isRollback {
			// Rollback: newest first (descending order)
			sort.Slice(pending, func(i, j int) bool {
				return pending[i].id > pending[j].id
			})
		} else {
			// Forward: oldest first (ascending order)
			sort.Slice(pending, func(i, j int) bool {
				return pending[i].id < pending[j].id
			})
		}

		// Apply migrations (read contents when applying)
		for _, m := range pending {
			contents, err := readMigrationContents(m.path)
			if err != nil {
				return err
			}
			if err := applyMigration(ctx, &tx, m.id, m.name, contents, isRollback); err != nil {
				return err
			}
		}

		if len(pending) == 0 && len(changedSignatures) == 0 {
			log.Info().Msg("Database schema up to date, no migrations applied")
		}

		return nil
	})
}
