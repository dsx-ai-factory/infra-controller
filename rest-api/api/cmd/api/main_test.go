// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestRunWithTracingShutdown(t *testing.T) {
	runErr := errors.New("run failed")
	shutdownErr := errors.New("shutdown failed")
	tests := []struct {
		descr       string
		runErr      error
		shutdownErr error
	}{
		{descr: "success"},
		{descr: "run failure", runErr: runErr},
		{descr: "shutdown failure", shutdownErr: shutdownErr},
		{descr: "both failures", runErr: runErr, shutdownErr: shutdownErr},
	}

	for _, tc := range tests {
		t.Run(tc.descr, func(t *testing.T) {
			calls := make([]string, 0, 2)
			err := runWithTracingShutdown(func() error {
				calls = append(calls, "run")
				return tc.runErr
			}, func(context.Context) error {
				calls = append(calls, "shutdown")
				return tc.shutdownErr
			})

			assert.Equal(t, []string{"run", "shutdown"}, calls)
			assert.Equal(t, tc.runErr != nil, errors.Is(err, runErr))
			assert.Equal(t, tc.shutdownErr != nil, errors.Is(err, shutdownErr))
		})
	}
}
