// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package executor

import (
	"context"
	"errors"
	"fmt"
)

// ErrRetryable identifies an operational executor failure that may succeed on
// a later attempt.
var ErrRetryable = errors.New("retryable executor error")

// ErrTerminal identifies an executor failure that cannot succeed by retrying
// the same request.
var ErrTerminal = errors.New("terminal executor error")

type classifiedError struct {
	classification error
	err            error
}

func (e classifiedError) Error() string {
	return e.err.Error()
}

func (e classifiedError) Unwrap() []error {
	return []error{e.classification, e.err}
}

func retryableError(operation string, err error) error {
	return Retryable(fmt.Errorf("%s: %w", operation, err))
}

// IsInterrupted reports whether err represents context cancellation or a
// context deadline.
func IsInterrupted(err error) bool {
	return errors.Is(err, context.Canceled) ||
		errors.Is(err, context.DeadlineExceeded)
}

// Retryable classifies err as an operational failure that may succeed on a
// later attempt. Context cancellation and deadline errors remain interruption
// errors rather than retryable failures.
func Retryable(err error) error {
	if err == nil ||
		IsInterrupted(err) ||
		errors.Is(err, ErrRetryable) ||
		errors.Is(err, ErrTerminal) {
		return err
	}

	return classifiedError{classification: ErrRetryable, err: err}
}

func terminalError(err error) error {
	return Terminal(err)
}

// Terminal classifies err as a failure that cannot succeed by retrying the
// same request.
func Terminal(err error) error {
	if err == nil || errors.Is(err, ErrTerminal) || errors.Is(err, ErrRetryable) {
		return err
	}

	return classifiedError{classification: ErrTerminal, err: err}
}
