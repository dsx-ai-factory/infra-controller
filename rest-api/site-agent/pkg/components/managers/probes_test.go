// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
)

func TestDependencyCheck_Run(t *testing.T) {
	checkErr := errors.New("connection refused")
	tests := []struct {
		name string
		// results are what successive checks return, one per run.
		results []error
		// after is how long after the first run each later run happens.
		after     []time.Duration
		wantErrs  []error
		wantCalls int
	}{
		{
			name:      "success is reused within the TTL",
			results:   []error{nil},
			after:     []time.Duration{readinessCacheTTL - time.Second},
			wantErrs:  []error{nil, nil},
			wantCalls: 1,
		},
		{
			name:      "success is checked again after the TTL",
			results:   []error{nil, checkErr},
			after:     []time.Duration{readinessCacheTTL},
			wantErrs:  []error{nil, checkErr},
			wantCalls: 2,
		},
		{
			name:      "failure is checked again on the next run",
			results:   []error{checkErr, nil},
			after:     []time.Duration{time.Second},
			wantErrs:  []error{checkErr, nil},
			wantCalls: 2,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			start := time.Now()
			now := start
			calls := 0
			dependency := &dependencyCheck{
				name: "test",
				check: func(ctx context.Context) error {
					deadline, ok := ctx.Deadline()
					require.True(t, ok)
					assert.WithinDuration(t, time.Now().Add(readinessCheckTimeout), deadline, time.Second)
					calls++
					return tt.results[calls-1]
				},
				now: func() time.Time { return now },
			}
			offsets := append([]time.Duration{0}, tt.after...)
			for i, offset := range offsets {
				now = start.Add(offset)
				assert.ErrorIs(t, dependency.run(context.Background()), tt.wantErrs[i], "run %d", i)
			}
			assert.Equal(t, tt.wantCalls, calls)
		})
	}
}

func TestProbes_HandleLiveness(t *testing.T) {
	tests := []struct {
		name     string
		err      error
		wantCode int
		wantBody string
	}{
		{name: "worker running", wantCode: http.StatusOK, wantBody: "ok\n"},
		{
			name:     "worker stopped",
			err:      errors.New("task queue name cannot start with reserved prefix /_sys/"),
			wantCode: http.StatusServiceUnavailable,
			wantBody: "Temporal worker is not running: task queue name cannot start with reserved prefix /_sys/\n",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			p := &probes{liveness: func() error { return tt.err }}
			response := httptest.NewRecorder()
			p.handleLiveness(response, httptest.NewRequest(http.MethodGet, computils.LivenessStatus, nil))
			assert.Equal(t, tt.wantCode, response.Code)
			assert.Equal(t, tt.wantBody, response.Body.String())
		})
	}
}

func TestProbes_HandleReadiness(t *testing.T) {
	tests := []struct {
		name        string
		temporalErr error
		coreErr     error
		wantCode    int
		wantBody    string
	}{
		{name: "every dependency reachable", wantCode: http.StatusOK, wantBody: "ok\n"},
		{
			name:        "every failing dependency is reported",
			temporalErr: errors.New("health check error: connection refused"),
			coreErr:     errors.New("context deadline exceeded"),
			wantCode:    http.StatusServiceUnavailable,
			wantBody:    "Temporal: health check error: connection refused\nCore gRPC: context deadline exceeded\n",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			p := &probes{dependencies: []*dependencyCheck{
				{name: "Temporal", check: func(context.Context) error { return tt.temporalErr }, now: time.Now},
				{name: "Core gRPC", check: func(context.Context) error { return tt.coreErr }, now: time.Now},
			}}
			response := httptest.NewRecorder()
			p.handleReadiness(response, httptest.NewRequest(http.MethodGet, computils.ReadinessStatus, nil))
			assert.Equal(t, tt.wantCode, response.Code)
			assert.Equal(t, tt.wantBody, response.Body.String())
		})
	}
}
