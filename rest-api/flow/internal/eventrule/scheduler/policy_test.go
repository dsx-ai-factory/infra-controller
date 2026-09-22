// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/executor"
)

func TestDefaultPolicyConfig(t *testing.T) {
	expected := PolicyConfig{
		MaxAttempts:  4,
		InitialDelay: 10 * time.Second,
		MaxDelay:     time.Minute,
	}

	actual := DefaultPolicyConfig()

	require.Equal(t, expected, actual)
	require.NoError(t, actual.Validate())
}

func TestPolicyConfig_resultForExecutionError(t *testing.T) {
	policy := PolicyConfig{
		MaxAttempts:  4,
		InitialDelay: 10 * time.Second,
		MaxDelay:     time.Minute,
	}
	tests := map[string]struct {
		attempts       int
		executionErr   error
		wantStatus     eventrule.ExecutionStatus
		wantReason     eventrule.ExecutionReason
		wantMessage    string
		wantRetryAfter time.Duration
	}{
		"retryable": {
			attempts:       1,
			executionErr:   executor.Retryable(errors.New("temporarily unavailable")),
			wantStatus:     eventrule.ExecutionStatusDeferred,
			wantReason:     eventrule.ExecutionReasonAttemptFailed,
			wantMessage:    "temporarily unavailable",
			wantRetryAfter: 10 * time.Second,
		},
		"canceled": {
			attempts:       1,
			executionErr:   context.Canceled,
			wantStatus:     eventrule.ExecutionStatusDeferred,
			wantReason:     eventrule.ExecutionReasonAttemptInterrupted,
			wantMessage:    context.Canceled.Error(),
			wantRetryAfter: 10 * time.Second,
		},
		"deadline exceeded": {
			attempts:       2,
			executionErr:   context.DeadlineExceeded,
			wantStatus:     eventrule.ExecutionStatusDeferred,
			wantReason:     eventrule.ExecutionReasonAttemptInterrupted,
			wantMessage:    context.DeadlineExceeded.Error(),
			wantRetryAfter: 20 * time.Second,
		},
		"interruption does not exhaust retry limit": {
			attempts:       4,
			executionErr:   context.Canceled,
			wantStatus:     eventrule.ExecutionStatusDeferred,
			wantReason:     eventrule.ExecutionReasonAttemptInterrupted,
			wantMessage:    context.Canceled.Error(),
			wantRetryAfter: time.Minute,
		},
		"terminal": {
			attempts:     1,
			executionErr: executor.Terminal(errors.New("invalid request")),
			wantStatus:   eventrule.ExecutionStatusFailed,
			wantMessage:  "invalid request",
		},
		"unclassified": {
			attempts:     1,
			executionErr: errors.New("unexpected response"),
			wantStatus:   eventrule.ExecutionStatusFailed,
			wantMessage:  "unexpected response",
		},
		"retry limit reached": {
			attempts:     4,
			executionErr: executor.Retryable(errors.New("still unavailable")),
			wantStatus:   eventrule.ExecutionStatusFailed,
			wantMessage:  "execution failed after 4 attempts: still unavailable",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			result := policy.resultForExecutionError(test.attempts, test.executionErr)

			require.Equal(t, test.wantStatus, result.Status)
			require.Equal(t, test.wantReason, result.Reason)
			require.Equal(t, test.wantMessage, result.StatusMessage)
			require.Equal(t, test.wantRetryAfter, result.RetryAfter)
		})
	}
}

func TestPolicyConfig_retryDelay(t *testing.T) {
	policy := PolicyConfig{
		InitialDelay: 10 * time.Second,
		MaxDelay:     45 * time.Second,
	}
	tests := map[string]struct {
		attempts int
		want     time.Duration
	}{
		"first attempt":       {attempts: 1, want: 10 * time.Second},
		"second attempt":      {attempts: 2, want: 20 * time.Second},
		"third attempt":       {attempts: 3, want: 40 * time.Second},
		"bounded":             {attempts: 4, want: 45 * time.Second},
		"large attempt count": {attempts: 100, want: 45 * time.Second},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			require.Equal(t, test.want, policy.retryDelay(test.attempts))
		})
	}
}
