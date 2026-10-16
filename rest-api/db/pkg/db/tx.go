// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package db

import (
	"context"
	"database/sql"
	"errors"
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

// WithSavepoint runs fn inside a savepoint of tx. If fn returns an error, tx
// rolls back to the savepoint, releases it, and returns that error. This also
// releases any advisory lock fn acquired, and clears a failed statement that
// would otherwise abort tx. If fn returns nil, the savepoint is released and
// its writes and locks stay held until tx ends.
//
// Use it to take a lock on a candidate that may be rejected, so the lock does
// not outlive the rejection. Failures of the savepoint statements themselves
// are tagged with ErrTransactionSavepoint.
//
// A successful fn that writes or locks rows keeps a subtransaction ID until tx
// ends. Postgres caches only 64 per backend before snapshots on every
// connection slow down, so keep such calls in one tx below that.
func (tx *Tx) WithSavepoint(ctx context.Context, fn func(sp *Tx) error) error {
	sp, err := tx.tx.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("%w: %w", ErrTransactionSavepoint, err)
	}
	err = fn(&Tx{tx: sp})
	if err != nil {
		// ROLLBACK TO SAVEPOINT leaves the savepoint open, so it is released too.
		// Otherwise each rejected fn nests the next savepoint one level deeper until tx ends.
		rerr := sp.Rollback()
		if rerr == nil {
			rerr = sp.Commit()
		}
		if rerr != nil {
			return errors.Join(err, fmt.Errorf("%w: %w", ErrTransactionSavepoint, rerr))
		}
		return err
	}
	err = sp.Commit()
	if err != nil {
		return fmt.Errorf("%w: %w", ErrTransactionSavepoint, err)
	}
	return nil
}

// Commit wraps bun's Commit
func (tx *Tx) Commit() error {
	return tx.tx.Commit()
}

// Rollback wraps bun's Rollback
func (tx *Tx) Rollback() error {
	return tx.tx.Rollback()
}

// AcquireAdvisoryLock will "try" to take the specified advisory lock
// on the transaction
// Error case:
// -----------
// if the lock is already held by another transaction, this will
// error, and the caller needs to (possibly) retry in the same transaction (after a delay)
// this is the api-handler usecase
// or retry in a new transaction after rolling back the current transaction
// this is the workflow worker usecase
// Success case:
// -------------
// the transaction lock when acquired is automatically released
// when the transaction commits or rollsback (or the transaction connection dies
// which is equivalent to a rollback for the transaction)
func (tx *Tx) AcquireAdvisoryLock(ctx context.Context, lockID uint64, blocking bool) error {

	if blocking {
		query := fmt.Sprintf("SELECT pg_advisory_xact_lock(%d)", lockID)
		_, err := tx.tx.Exec(query)
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
