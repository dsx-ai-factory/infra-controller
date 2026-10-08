// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestHealthCheckHandler_Handle(t *testing.T) {
	type args struct {
		c echo.Context
	}

	e := echo.New()
	req := httptest.NewRequest(http.MethodPost, "/", nil)
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()

	tests := []struct {
		name string
		hch  HealthCheckHandler
		args args
	}{
		{
			name: "test health check API endpoint",
			hch:  HealthCheckHandler{},
			args: args{
				c: e.NewContext(req, rec),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			hch := HealthCheckHandler{}
			err := hch.Handle(tt.args.c)
			assert.NoError(t, err)

			assert.Equal(t, http.StatusOK, rec.Code)

			rhc := &model.APIHealthCheck{}

			serr := json.Unmarshal(rec.Body.Bytes(), rhc)
			assert.NoError(t, serr)

			assert.Equal(t, true, rhc.IsHealthy)
		})
	}
}

func TestReadinessCheckHandler_Handle(t *testing.T) {
	tests := []struct {
		name          string
		prepare       func(*testing.T, *cdb.Session) func()
		cancelRequest bool
		wantStatus    int
		wantBody      string
	}{
		{
			name:       "healthy database",
			wantStatus: http.StatusOK,
			wantBody:   `{"is_healthy":true,"error":null}`,
		},
		{
			name: "closed database returns a sanitized error",
			prepare: func(_ *testing.T, session *cdb.Session) func() {
				session.Close()
				return nil
			},
			wantStatus: http.StatusServiceUnavailable,
			wantBody:   `{"is_healthy":false,"error":"database connection is unavailable"}`,
		},
		{
			name: "exhausted pool times out and recovers when released",
			prepare: func(t *testing.T, session *cdb.Session) func() {
				session.DB.SetMaxOpenConns(1)
				conn, err := session.DB.DB.Conn(t.Context())
				require.NoError(t, err)
				release := func() {
					if conn != nil {
						assert.NoError(t, conn.Close())
						conn = nil
					}
				}
				t.Cleanup(release)
				return func() {
					assert.Positive(t, session.DB.Stats().WaitCount)
					release()
				}
			},
			wantStatus: http.StatusServiceUnavailable,
			wantBody:   `{"is_healthy":false,"error":"database connection is unavailable"}`,
		},
		{
			name: "read-only connection is rejected and replaced",
			prepare: func(t *testing.T, session *cdb.Session) func() {
				session.DB.SetMaxOpenConns(1)
				session.DB.SetMaxIdleConns(1)
				var originalPID int
				err := session.DB.QueryRowContext(t.Context(), "SELECT pg_backend_pid()").Scan(&originalPID)
				require.NoError(t, err)
				_, err = session.DB.ExecContext(t.Context(), "SET default_transaction_read_only = on")
				require.NoError(t, err)
				err = session.DB.PingContext(t.Context())
				require.NoError(t, err, "read-only PostgreSQL still responds to a ping")
				return func() {
					var replacementPID int
					err := session.DB.QueryRowContext(t.Context(), "SELECT pg_backend_pid()").Scan(&replacementPID)
					require.NoError(t, err)
					assert.NotEqual(t, originalPID, replacementPID, "discard the physical connection, not just its SQL wrapper")
				}
			},
			wantStatus: http.StatusServiceUnavailable,
			wantBody:   `{"is_healthy":false,"error":"database connection is unavailable"}`,
		},
		{
			name:          "request cancellation stops the check",
			cancelRequest: true,
			wantStatus:    http.StatusServiceUnavailable,
			wantBody:      `{"is_healthy":false,"error":"database connection is unavailable"}`,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			session := cdbu.GetTestDBSession(t, false)
			t.Cleanup(session.Close)
			var recoverDatabase func()
			if tt.prepare != nil {
				recoverDatabase = tt.prepare(t, session)
			}

			ctx, cancel := context.WithTimeout(t.Context(), 2*time.Second)
			defer cancel()
			if tt.cancelRequest {
				cancel()
			}
			e := echo.New()
			req := httptest.NewRequestWithContext(ctx, http.MethodGet, "/readyz", nil)
			rec := httptest.NewRecorder()
			handler := NewReadinessCheckHandler(session)
			started := time.Now()
			err := handler.Handle(e.NewContext(req, rec))
			require.NoError(t, err)
			assert.Less(t, time.Since(started), time.Second)
			assert.Equal(t, tt.wantStatus, rec.Code)
			assert.JSONEq(t, tt.wantBody, rec.Body.String())

			if recoverDatabase != nil {
				recoverDatabase()
				rec = httptest.NewRecorder()
				err = handler.Handle(e.NewContext(req, rec))
				require.NoError(t, err)
				assert.Equal(t, http.StatusOK, rec.Code)
				assert.JSONEq(t, `{"is_healthy":true,"error":null}`, rec.Body.String())
			}
		})
	}
}
