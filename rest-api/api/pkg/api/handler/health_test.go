// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/dsx-ai-factory/infra-controller/rest-api/api/pkg/api/model"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
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
