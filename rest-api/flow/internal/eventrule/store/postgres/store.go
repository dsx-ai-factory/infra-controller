// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package postgres persists event rules, durable events, and executions in
// PostgreSQL.
package postgres

import (
	"context"
	"database/sql"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/uptrace/bun"
)

// Store is the PostgreSQL event-rule persistence boundary shared by the
// processor, scheduler, and executors.
type Store struct {
	pg         *cdb.Session
	timestamps timestampSource
}

type timestampSource interface {
	Timestamp(context.Context, bun.IDB) (time.Time, error)
}

type postgresTimestampSource struct{}

func (postgresTimestampSource) Timestamp(
	ctx context.Context,
	db bun.IDB,
) (time.Time, error) {
	var now time.Time
	err := db.NewSelect().ColumnExpr("CURRENT_TIMESTAMP").Scan(ctx, &now)

	return now.UTC(), err
}

// New constructs a PostgreSQL-backed event-rule store.
func New(pg *cdb.Session) *Store {
	return &Store{
		pg:         pg,
		timestamps: postgresTimestampSource{},
	}
}

func (s *Store) runInTx(
	ctx context.Context,
	fn func(context.Context, bun.Tx) error,
) error {
	return s.pg.DB.RunInTx(ctx, &sql.TxOptions{}, fn) //nolint:exhaustruct,wrapcheck
}

func (s *Store) timestamp(ctx context.Context, db bun.IDB) (time.Time, error) {
	return s.timestamps.Timestamp(ctx, db)
}

var _ eventrule.Store = (*Store)(nil)
