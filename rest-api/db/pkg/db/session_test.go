// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package db

import (
	"context"
	"fmt"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.opentelemetry.io/otel"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
)

func TestNewSession(t *testing.T) {
	ctx := context.Background()
	type args struct {
		host       string
		port       int
		dbName     string
		user       string
		password   string
		caCertPath string
	}
	tests := []struct {
		name    string
		args    args
		want    *Session
		wantErr bool
	}{
		{
			name: "create a DB session",
			args: args{
				host:       "localhost",
				port:       5432,
				dbName:     "postgres",
				user:       "postgres",
				password:   "postgres",
				caCertPath: "",
			},
			want:    &Session{},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := NewSession(ctx, tt.args.host, tt.args.port, tt.args.dbName, tt.args.user, tt.args.password, tt.args.caCertPath)
			if (err != nil) != tt.wantErr {
				t.Errorf("NewSession() error = %v, wantErr %v", err, tt.wantErr)
				return
			}

			if got == nil {
				t.Errorf("NewSession() failed to init DB session")
			}
		})
	}
}

// TestTracingQueryHook proves the query hook follows the tracing bootstrap:
// it is attached only once a real tracer provider was installed, so a session
// created while tracing is off does not pay for a hook feeding a no-op
// provider.
func TestTracingQueryHook(t *testing.T) {
	tests := []struct {
		descr    string
		endpoint string
		wantHook bool
	}{
		{descr: "no tracer provider installed"},
		{descr: "tracer provider installed", endpoint: "http://localhost:14318", wantHook: true},
	}

	for _, tc := range tests {
		t.Run(tc.descr, func(t *testing.T) {
			previousProvider := otel.GetTracerProvider()
			previousPropagator := otel.GetTextMapPropagator()
			t.Cleanup(func() {
				otel.SetTracerProvider(previousProvider)
				otel.SetTextMapPropagator(previousPropagator)
			})
			t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", tc.endpoint)
			t.Setenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "")
			t.Setenv("OTEL_TRACES_SAMPLER", "always_off")
			shutdown, err := cotel.Bootstrap(context.Background(), true, "db-test")
			require.NoError(t, err)
			t.Cleanup(func() { require.NoError(t, shutdown(context.Background())) })

			hook := tracingQueryHook("nico")

			assert.Equal(t, tc.wantHook, hook != nil)
		})
	}
}

// Demonstrates the 2 problems with session advisory locks due to the connection pool
// in database/sql
func TestSessionAcquireAdvisoryLock(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	ctx := context.Background()
	tests := []struct {
		name      string
		expectErr bool
		testcase  int
	}{
		{
			name:     "success acquire lock",
			testcase: 1,
		},
		{
			name:     "PROBLEM: can re-acquire lock from same session",
			testcase: 2,
		},
		{
			name:     "PROBLEM: unlock failure because unlock was attempted in another connection",
			testcase: 3,
		},
		{
			name:     "success, lock acquire from another session fails",
			testcase: 4,
		},
	}

	var err error
	c := make(chan int, 1)
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			switch tc.testcase {
			case 1:
				// success acquire lock
				err = dbSession.acquireAdvisoryLock(ctx, uint64(123))
				assert.Nil(t, err)
			case 2:
				// PROBLEM !! can reacquire same lock (since the same connection
				// is most likely used in database/sql)
				err = dbSession.acquireAdvisoryLock(ctx, uint64(123))
				assert.Nil(t, err)
			case 3:
				// PROBLEM: unlock failure because unlock was attempted in another connection
				// which didnt have the lock
				// launch 3 long running query in a goroutine to hog connections in conn pool
				for i := 0; i < 2; i++ {
					go func() {
						_, err := dbSession.DB.Exec("select pg_sleep(2)")
						assert.Nil(t, err)
						c <- 1
					}()
				}
				time.Sleep(1 * time.Second)
				// meanwhile attempt to unlock the lock would fail because the connection is different
				// from the one that acquired the lock
				err = dbSession.releaseAdvisoryLock(ctx, uint64(123))
				assert.NotNil(t, err)
				fmt.Println(err)
			case 4:
				// lock acquire from another session fails because lock is still being held
				err = dbSession.acquireAdvisoryLock(ctx, uint64(123))
				assert.NotNil(t, err)

				for i := 0; i < 2; i++ {
					<-c
				}
			}
		})
	}
}
