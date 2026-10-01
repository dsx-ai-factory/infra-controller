// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package db

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"github.com/avast/retry-go/v4"
	"github.com/uptrace/bun"
)

const (
	defaultRetries              = 3   // number of retries for lock acquisition
	defaultRetryDelay           = 300 // in ms
	defaultRetryMaxJitter       = 100 // in ms
	DefaultTxLockTimeoutSeconds = 300 // in seconds
)

type LockRetryOptions struct {
	Retries *int
	Delay   *time.Duration
	Jitter  *time.Duration
}

// Tx is a thin wrapper around the bun.Tx object
type Tx struct {
	tx bun.Tx
}

// BeginTx wraps bun's BeginTx
func BeginTx(ctx context.Context, dbSession *Session, txOptions *sql.TxOptions) (*Tx, error) {
	tx, err := dbSession.DB.BeginTx(ctx, txOptions)
	if err != nil {
		return nil, err
	}

	// Set a max lock timeout to 300s for the transaction
	// so that no blocking attempt to acquire a lock
	// will block indefinitely.
	_, err = tx.Exec(fmt.Sprintf("SET LOCAL lock_timeout = '%ds'", DefaultTxLockTimeoutSeconds))
	if err != nil {
		tx.Rollback()
		return nil, err
	}

	return &Tx{
		tx: tx,
	}, nil
}

// RollbackTx is called deferred in functions that create a transaction
// if transaction was committed, this will do nothing
func RollbackTx(ctx context.Context, tx *Tx, committed *bool) {
	if committed != nil && !*committed {
		tx.Rollback()
	}
}

// WithTx runs fn inside a database transaction. If fn returns nil, the
// transaction is committed; if fn returns an error, the transaction is rolled
// back. Callers don't have to manage Begin/Commit/Rollback or a "did we
// commit?" flag manually.
//
// Use this in preference to BeginTx + manual RollbackTx + tx.Commit() at call
// sites. See WithTxOpts for non-default tx options.
func WithTx(ctx context.Context, dbSession *Session, fn func(tx *Tx) error) error {
	return WithTxOpts(ctx, dbSession, &sql.TxOptions{}, fn)
}

// WithTxOpts is the variant of WithTx that lets callers pass non-default
// sql.TxOptions (e.g., a specific isolation level).
func WithTxOpts(ctx context.Context, dbSession *Session, opts *sql.TxOptions, fn func(tx *Tx) error) error {
	tx, err := BeginTx(ctx, dbSession, opts)
	if err != nil {
		return fmt.Errorf("%w: %w", ErrTransactionInitiation, err)
	}
	// If fn panics, ensure the tx is rolled back so we don't leak an open
	// transaction (and any locks it holds) until the connection drops.
	// The original panic is re-raised after rollback.
	defer func() {
		if p := recover(); p != nil {
			_ = tx.Rollback()
			panic(p)
		}
	}()
	if err := fn(tx); err != nil {
		_ = tx.Rollback()
		return err
	}
	if err := tx.Commit(); err != nil {
		return fmt.Errorf("%w: %w", ErrTransactionCommit, err)
	}
	return nil
}

// WithTxResult is the value-returning variant of WithTx. The closure can
// return a result of any type; it's only returned to the caller if the
// transaction committed successfully.
func WithTxResult[T any](ctx context.Context, dbSession *Session, fn func(tx *Tx) (T, error)) (T, error) {
	return WithTxResultOpts(ctx, dbSession, &sql.TxOptions{}, fn)
}

// WithTxResultOpts is the value-returning variant of WithTxOpts.
func WithTxResultOpts[T any](ctx context.Context, dbSession *Session, opts *sql.TxOptions, fn func(tx *Tx) (T, error)) (T, error) {
	var zero T
	tx, err := BeginTx(ctx, dbSession, opts)
	if err != nil {
		return zero, fmt.Errorf("begin tx: %w", err)
	}
	// If fn panics, ensure the tx is rolled back so we don't leak an open
	// transaction (and any locks it holds) until the connection drops.
	// The original panic is re-raised after rollback.
	defer func() {
		if p := recover(); p != nil {
			_ = tx.Rollback()
			panic(p)
		}
	}()
	result, err := fn(tx)
	if err != nil {
		_ = tx.Rollback()
		return zero, err
	}
	if err := tx.Commit(); err != nil {
		return zero, fmt.Errorf("%w: %w", ErrTransactionCommit, err)
	}
	return result, nil
}

// Commit wraps bun's Commit
func (tx *Tx) Commit() error {
	return tx.tx.Commit()
}

// Rollback wraps bun's Rollback
func (tx *Tx) Rollback() error {
	return tx.tx.Rollback()
}

// AcquireAdvisoryLock acquires a transaction-scoped advisory lock. Blocking
// acquisition waits until the lock is available or ctx is canceled.
// Non-blocking acquisition returns ErrXactAdvisoryLockFailed when another
// transaction holds the lock. The lock is released automatically when the
// transaction commits, rolls back, or loses its connection.
func (tx *Tx) AcquireAdvisoryLock(ctx context.Context, lockID uint64, blocking bool) error {

	if blocking {
		query := fmt.Sprintf("SELECT pg_advisory_xact_lock(%d)", lockID)
		_, err := tx.tx.ExecContext(ctx, query)
		return err
	}

	query := fmt.Sprintf("pg_try_advisory_xact_lock(%d)", lockID)
	value := false
	err := tx.tx.NewSelect().ColumnExpr(query).Scan(ctx, &value)

	if err != nil {
		return err
	}
	if !value {
		return ErrXactAdvisoryLockFailed
	}
	return nil
}

// GetBunTx gets the bun transaction object
func (tx *Tx) GetBunTx() *bun.Tx {
	return &tx.tx
}

// GetAdvisoryLockIDFromString returns the advisory lock ID from a string
// pg expects lockid to not have the msb set
func GetAdvisoryLockIDFromString(id string) uint64 {
	n := GetStringToUint64Hash(id)
	return n & uint64(0x7fffffffffffffff)
}

// GetIDB is used by DAO methods to get the DB interface
// If DAO method's tx parameter is non-nil, return it
// else return the dbSession
// note: both bun.Tx and bun.DB implement the bun.IDB
func GetIDB(tx *Tx, dbSession *Session) bun.IDB {
	if tx != nil {
		return tx.tx
	}
	return dbSession.DB
}

// TryAcquireAdvisoryLock acquires an advisory lock
// retrying (up to retryCnt times which defaults to 3) when the lock acquisition attempt fails
// note, that each lock acquisition attempt is a non-blocking pg_try_advisory_xact_lock
// retries will backoff exponentially starting with initial delay of 300ms (300ms, 600ms, 1200ms etc..)
// with a max-jitter of 100ms.
func (tx *Tx) TryAcquireAdvisoryLock(ctx context.Context, lockID uint64, options *LockRetryOptions) error {
	retriableFunc := func() error {
		return tx.AcquireAdvisoryLock(ctx, lockID, false)
	}

	if options == nil {
		options = &LockRetryOptions{}
	}

	delay := defaultRetryDelay * time.Millisecond
	jitter := defaultRetryMaxJitter * time.Millisecond

	if options.Delay != nil {
		delay = *options.Delay
	}

	if options.Jitter != nil {
		jitter = *options.Jitter
	}

	retries := uint(defaultRetries)
	if options.Retries != nil {
		retries = uint(*options.Retries)
	}
	return retry.Do(
		retriableFunc,
		retry.Attempts(retries),
		retry.Delay(delay),
		retry.MaxJitter(jitter),
		retry.DelayType(retry.CombineDelay(retry.BackOffDelay, retry.RandomDelay)),
	)
}
