// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package testutil

import (
	"context"
	"fmt"
	"strings"
	"testing"

	"github.com/jackc/pgx/v5/pgxpool"
	log "github.com/sirupsen/logrus"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

// CreateTestDB creates a fresh test database for integration tests.
// It creates a new database with a unique name based on the test name
// and returns the connection.
func CreateTestDB(ctx context.Context, t *testing.T, dbConf db.Config) (*db.Session, error) {
	// Connect to the main database first to create the test database
	dbInitial, err := pgxpool.New(ctx, dbConf.BuildDSN())
	if err != nil {
		return nil, err
	}
	defer dbInitial.Close()

	testDBName := testDatabaseName(dbConf.DBName, t.Name())
	log.Infof("Creating test database: %v", testDBName)

	// Quote the database name as a PostgreSQL identifier to prevent SQL injection
	quotedDBName := QuoteIdentifier(testDBName)

	// Drop existing test database if it exists
	if _, err = dbInitial.Exec(ctx, "DROP DATABASE IF EXISTS "+quotedDBName); err != nil {
		return nil, err
	}

	// Create new test database
	if _, err = dbInitial.Exec(ctx, "CREATE DATABASE "+quotedDBName); err != nil {
		return nil, err
	}

	// Connect to the new test database
	dbConfNew := dbConf
	dbConfNew.DBName = testDBName

	session, err := db.NewSessionFromConfig(ctx, dbConfNew)
	if err != nil {
		return nil, fmt.Errorf("failed to connect to test DB %s: %w", testDBName, err)
	}

	return session, nil
}

func testDatabaseName(dbName, testName string) string {
	testName = strings.ToLower(strings.ReplaceAll(testName, "/", "_"))
	name := dbName + "_test_" + testName
	// PostgreSQL limits identifiers to 63 bytes.
	if len(name) <= 63 {
		return name
	}

	// Hash the full name so subtests that differ in the shortened portion
	// still get separate databases. Drop any partial UTF-8 character from the prefix.
	prefix := strings.ToValidUTF8(name[:63-1-16], "")
	return fmt.Sprintf("%s_%016x", prefix, db.GetStringToUint64Hash(name))
}

// QuoteIdentifier quotes a string as a PostgreSQL identifier.
// It wraps the identifier in double quotes and escapes any internal double quotes.
func QuoteIdentifier(name string) string {
	return `"` + strings.ReplaceAll(name, `"`, `""`) + `"`
}
