// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package executor

import (
	"context"
	"errors"
	"fmt"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestErrorClassification(t *testing.T) {
	original := errors.New("downstream unavailable")
	tests := map[string]struct {
		err            error
		classification error
		original       error
		wantMessage    string
	}{
		"retryable": {
			err: Retryable(original), classification: ErrRetryable,
			original: original, wantMessage: original.Error(),
		},
		"terminal": {
			err: Terminal(original), classification: ErrTerminal,
			original: original, wantMessage: original.Error(),
		},
		"context cancellation remains interruption": {
			err: Retryable(context.Canceled), original: context.Canceled,
			wantMessage: context.Canceled.Error(),
		},
		"nil remains nil": {},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			if test.err == nil {
				require.NoError(t, test.err)
				return
			}

			require.EqualError(t, test.err, test.wantMessage)
			require.ErrorIs(t, test.err, test.original)

			if test.classification != nil {
				require.ErrorIs(t, test.err, test.classification)
			} else {
				require.NotErrorIs(t, test.err, ErrRetryable)
				require.NotErrorIs(t, test.err, ErrTerminal)
			}
		})
	}
}

func TestIsInterrupted(t *testing.T) {
	tests := map[string]struct {
		err  error
		want bool
	}{
		"nil":               {},
		"canceled":          {err: context.Canceled, want: true},
		"wrapped canceled":  {err: fmt.Errorf("execute: %w", context.Canceled), want: true},
		"deadline":          {err: context.DeadlineExceeded, want: true},
		"wrapped deadline":  {err: fmt.Errorf("execute: %w", context.DeadlineExceeded), want: true},
		"unrelated failure": {err: errors.New("downstream unavailable")},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			require.Equal(t, test.want, IsInterrupted(test.err))
		})
	}
}
