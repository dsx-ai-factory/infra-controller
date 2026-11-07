// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"context"
	"database/sql"
	"errors"

	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/jackc/pgx/v5/stdlib"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/db"
)

// Postgres wraps a Bun DB connection and error checker for Postgres-specific behavior.
type Postgres struct {
	dbName       string
	db           *bun.DB
	errorChecker db.ErrorChecker
}

// New initializes a Postgres connection using a pgx pool and returns a helper.
func New(ctx context.Context, c db.Config) (*Postgres, error) {
	pool, err := pgxpool.New(ctx, c.BuildDSN())
	if err != nil {
		return nil, err
	}

	return &Postgres{
		dbName:       c.DBName,
		db:           bun.NewDB(stdlib.OpenDBFromPool(pool), pgdialect.New()),
		errorChecker: &PostgresErrorChecker{},
	}, nil
}

// Close closes the underlying Bun DB.
func (p *Postgres) Close(ctx context.Context) error {
	return p.db.Close()
}

// BeginTx begins a new transaction with default options.
func (p *Postgres) BeginTx(ctx context.Context) (bun.Tx, error) {
	return p.db.BeginTx(ctx, &sql.TxOptions{})
}

// RunInTx executes a function within a transaction.
func (p *Postgres) RunInTx(
	ctx context.Context,
	fn func(ctx context.Context, tx bun.Tx) error,
) error {
	return p.db.RunInTx(ctx, &sql.TxOptions{}, fn)
}

// ErrorChecker returns a Postgres error classifier.
func (p *Postgres) ErrorChecker() db.ErrorChecker {
	return p.errorChecker
}

// DB exposes the underlying Bun DB.
func (p *Postgres) DB() *bun.DB {
	return p.db
}

// PostgresErrorChecker classifies common Postgres errors such as no rows and unique constraint violations.
type PostgresErrorChecker struct{}

func (checker *PostgresErrorChecker) IsErrNoRows(err error) bool {
	return errors.Is(err, sql.ErrNoRows)
}

func (checker *PostgresErrorChecker) IsUniqueConstraintError(err error) bool {
	if err != nil {
		if pgErr, ok := err.(*pgconn.PgError); ok {
			if pgErr.Code == "23505" {
				return true
			}
		}
	}

	return false
}
