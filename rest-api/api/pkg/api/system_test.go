// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package api

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
)

func TestNewSystemAPIRoutes(t *testing.T) {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.Close()

	e := echo.New()
	for _, route := range NewSystemAPIRoutes(dbSession) {
		e.Add(route.Method, route.Path, route.Handler.Handle)
	}

	tests := []struct {
		name       string
		path       string
		wantStatus int
		wantBody   string
	}{
		{
			name:       "liveness ignores the unavailable database",
			path:       "/healthz",
			wantStatus: http.StatusOK,
			wantBody:   `{"is_healthy":true,"error":null}`,
		},
		{
			name:       "readiness checks the supplied database session",
			path:       "/readyz",
			wantStatus: http.StatusServiceUnavailable,
			wantBody:   `{"is_healthy":false,"error":"database connection is unavailable"}`,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := httptest.NewRecorder()
			e.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, tt.path, nil))
			assert.Equal(t, tt.wantStatus, rec.Code)
			assert.JSONEq(t, tt.wantBody, rec.Body.String())
		})
	}
}

func TestIsSystemRoute(t *testing.T) {
	dbSession := &cdb.Session{}
	tests := []struct {
		path string
		want bool
	}{
		{path: "/healthz", want: true},
		{path: "/readyz", want: true},
		{path: "/not-a-system-route", want: false},
	}
	for _, tt := range tests {
		t.Run(tt.path, func(t *testing.T) {
			assert.Equal(t, tt.want, IsSystemRoute(tt.path, dbSession))
		})
	}
}
