// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package db

import (
	"database/sql"
	"errors"
	"time"

	"github.com/jackc/pgx/v5/pgconn"
)

// ErrorChecker abstracts database error classification.
type ErrorChecker interface {
	IsErrNoRows(err error) bool
	IsUniqueConstraintError(err error) bool
}

// PostgresErrorChecker classifies common Postgres errors such as
// no rows and unique constraint violations.
type PostgresErrorChecker struct{}

func (checker *PostgresErrorChecker) IsErrNoRows(err error) bool {
	return errors.Is(err, sql.ErrNoRows)
}

func (checker *PostgresErrorChecker) IsUniqueConstraintError(err error) bool {
	var pgErr *pgconn.PgError
	if errors.As(err, &pgErr) {
		return pgErr.Code == "23505"
	}

	return false
}

// CurTime returns the current UTC time rounded to microseconds
// (useful for DB timestamps).
func CurTime() time.Time {
	return time.Now().UTC().Round(time.Microsecond)
}
