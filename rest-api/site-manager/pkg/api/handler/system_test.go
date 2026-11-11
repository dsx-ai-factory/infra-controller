// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
)

func TestSystemHandlers(t *testing.T) {
	tests := []struct {
		name     string
		handler  interface{ Handle(echo.Context) error }
		wantBody string
	}{
		{
			name:     "health answers ok",
			handler:  NewHealthCheckHandler(),
			wantBody: "ok\n",
		},
		{
			name:     "version answers the version and a newline",
			handler:  NewVersionHandler("v-test"),
			wantBody: "v-test\n",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := serve(t, tt.handler, "", "")
			assert.Equal(t, http.StatusOK, rec.Code)
			assert.Equal(t, tt.wantBody, rec.Body.String())
		})
	}
}
