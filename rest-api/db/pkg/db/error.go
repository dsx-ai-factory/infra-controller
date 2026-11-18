// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package db

import (
	"errors"
)

var (
	// ErrDoesNotExist is raised a DB query fails to find the requested entity
	ErrDoesNotExist = errors.New("the requested entity does not exist")
	// ErrDBError is a generalized error to expose to the user when unexpected errors occur when communicating with DB
	ErrDBError = errors.New("error communicating with data store")
	// ErrInvalidValue is raised when a value to be stored in DB is invalid
	ErrInvalidValue = errors.New("provided value is invalid")
	// ErrInvalidParams is raised when a function is called with invalid set of parameters
	ErrInvalidParams = errors.New("provided params are invalid or conflicting")

	// ErrXactAdvisoryLockFailed indicates that the transaction advisory lock could not be taken
	ErrXactAdvisoryLockFailed = errors.New("unable to take transaction advisory lock")
	// ErrSessionAdvisoryLockFailed indicates that the session advisory lock could not be taken
	ErrSessionAdvisoryLockFailed = errors.New("unable to take session advisory lock")
	// ErrSessionAdvisoryLockUnlockFailed indicates that the session advisory lock could not be released.
	ErrSessionAdvisoryLockUnlockFailed = errors.New("unable to release session advisory lock or lock was not held by this session")

	// ErrInvalidPort indicates the DB_PORT environment variable is not a valid integer.
	ErrInvalidPort = errors.New("failed to parse DB_PORT")
	// ErrInvalidCredential indicates the credential is not valid.
	ErrInvalidCredential = errors.New("invalid credential")

	// ErrTransactionInitiation is returned by WithTx*/WithTxResult* when the
	// underlying BeginTx call fails. HandleTxError detects this sentinel via
	// errors.Is and renders a user-facing message about transaction initiation.
	ErrTransactionInitiation = errors.New("DB transaction initiation error")
	// ErrTransactionCommit is returned by WithTx*/WithTxResult* when the
	// underlying tx.Commit call fails. HandleTxError detects this sentinel via
	// errors.Is and renders a user-facing message about transaction commit.
	ErrTransactionCommit = errors.New("DB transaction commit error")
	// ErrTransactionSavepoint is returned by Tx.WithSavepoint when creating,
	// rolling back to, or releasing the savepoint fails. The enclosing
	// transaction is no longer reliable, so callers should abort it.
	ErrTransactionSavepoint = errors.New("DB transaction savepoint error")
)
