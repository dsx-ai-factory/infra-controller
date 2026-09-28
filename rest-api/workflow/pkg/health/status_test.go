// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package health

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestStatusHandler(t *testing.T) {
	tests := []struct {
		name string
		path string
	}{
		{
			name: "health response",
			path: "/healthz",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			request := httptest.NewRequest(http.MethodGet, test.path, nil)
			response := httptest.NewRecorder()

			StatusHandler(response, request)

			require.Equal(t, http.StatusOK, response.Code)
			require.Equal(t, "application/json", response.Header().Get("Content-Type"))
			require.JSONEq(t, `{"is_healthy":true,"error":null}`, response.Body.String())
		})
	}
}
