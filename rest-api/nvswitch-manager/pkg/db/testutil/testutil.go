// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package testutil provides database test utilities for integration tests.
// This package is intentionally separate from production code to prevent
// test infrastructure from being compiled into production binaries.
package testutil

import (
	"context"
	"fmt"
	"strings"
	"testing"

	"github.com/jackc/pgx/v5/pgxpool"
	log "github.com/sirupsen/logrus"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/db/postgres"
)

// CreateTestDB creates a fresh test database for integration tests.
// It creates a new database with a unique name based on the test name
// and returns the connection.
func CreateTestDB(ctx context.Context, t *testing.T, dbConf db.Config) (*postgres.Postgres, error) {
	// Connect to the main database first to create the test database
	dbInitial, err := pgxpool.New(ctx, dbConf.BuildDSN())
	if err != nil {
		return nil, err
	}
	defer dbInitial.Close()

	// Create a unique test database name based on the test name
	// PostgreSQL has a 63-character limit for identifiers
	testName := strings.ToLower(strings.ReplaceAll(t.Name(), "/", "_"))
	testDBName := dbConf.DBName + "_test_" + testName

	// Truncate to 63 characters if needed, ensuring uniqueness with a hash suffix
	if len(testDBName) > 63 {
		// Use last 8 chars as a simple hash-like suffix for uniqueness
		// Guard against short test names (less than 8 chars)
		suffixLen := 8
		if len(testName) < suffixLen {
			suffixLen = len(testName)
		}
		suffix := testName[len(testName)-suffixLen:]
		maxPrefix := 63 - 1 - len(suffix) // 1 for "_" separator
		if maxPrefix > len(testDBName)-len(suffix)-1 {
			maxPrefix = len(testDBName) - len(suffix) - 1
		}
		if maxPrefix < 1 {
			maxPrefix = 1
		}
		testDBName = testDBName[:maxPrefix] + "_" + suffix
	}
	log.Infof("Creating test database: %v", testDBName)

	// Quote the database name as a PostgreSQL identifier to prevent SQL injection
	// PostgreSQL identifiers are quoted with double quotes, and internal double quotes are escaped by doubling
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

	pg, err := postgres.New(ctx, dbConfNew)
	if err != nil {
		return nil, fmt.Errorf("failed to connect to test DB %s: %w", testDBName, err)
	}

	return pg, nil
}

// QuoteIdentifier quotes a string as a PostgreSQL identifier.
// It wraps the identifier in double quotes and escapes any internal double quotes.
func QuoteIdentifier(name string) string {
	return `"` + strings.ReplaceAll(name, `"`, `""`) + `"`
}
