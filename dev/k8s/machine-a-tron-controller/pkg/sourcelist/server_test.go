// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sourcelist

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestHandler_SourcesNotReady(t *testing.T) {
	r := NewRegistry()
	rec := httptest.NewRecorder()

	Handler(r).ServeHTTP(rec, httptest.NewRequest(http.MethodGet, PathSources, nil))

	assert.Equal(t, http.StatusOK, rec.Code)
	assert.Equal(t, "application/json", rec.Header().Get("Content-Type"))
	assert.JSONEq(t, `{"generation":0,"ready":false,"sources":[]}`, rec.Body.String())
}

func TestHandler_SourcesGolden(t *testing.T) {
	// testdata/sources_v1.json is the cross-language contract fixture: the
	// Rust gateway's contract tests include the same file and check field
	// parity (crates/mat-protocol-gateway/tests/integration/source_list_contract.rs).
	// Keep the registry state below, the fixture and those tests in sync.
	r := NewRegistry(WithDebounce(0))
	r.Observe([]Source{
		{Name: "nico-machine-a-tron-single-bmc-mock", BaseURL: "https://nico-machine-a-tron-single-bmc-mock.nico-system.svc.cluster.local:1266"},
	})
	r.Observe([]Source{
		{Name: "nico-machine-a-tron-single-bmc-mock", BaseURL: "https://nico-machine-a-tron-single-bmc-mock.nico-system.svc.cluster.local:1266"},
		{Name: "nico-machine-a-tron-mat-0-bmc-mock", BaseURL: "https://nico-machine-a-tron-mat-0-bmc-mock.nico-system.svc.cluster.local:8443", Pod: "mat-0"},
	})
	r.Observe([]Source{
		{Name: "nico-machine-a-tron-mat-1-bmc-mock", BaseURL: "https://nico-machine-a-tron-mat-1-bmc-mock.nico-system.svc.cluster.local:8443", Pod: "mat-1"},
		{Name: "nico-machine-a-tron-single-bmc-mock", BaseURL: "https://nico-machine-a-tron-single-bmc-mock.nico-system.svc.cluster.local:1266"},
		{Name: "nico-machine-a-tron-mat-0-bmc-mock", BaseURL: "https://nico-machine-a-tron-mat-0-bmc-mock.nico-system.svc.cluster.local:8443", Pod: "mat-0"},
	})

	rec := httptest.NewRecorder()
	Handler(r).ServeHTTP(rec, httptest.NewRequest(http.MethodGet, PathSources, nil))
	require.Equal(t, http.StatusOK, rec.Code)

	want, err := os.ReadFile(filepath.Join("testdata", "sources_v1.json"))
	require.NoError(t, err)
	assert.JSONEq(t, string(want), rec.Body.String())

	// The fixture must round-trip through the Go types and preserve order.
	var decoded Response
	require.NoError(t, json.Unmarshal(want, &decoded))
	assert.Equal(t, r.Snapshot(), decoded)
}

func TestHandler_StatusCodes(t *testing.T) {
	r := NewRegistry()
	h := Handler(r)

	tests := []struct {
		name   string
		method string
		path   string
		want   int
	}{
		{"get sources", http.MethodGet, PathSources, http.StatusOK},
		{"head sources", http.MethodHead, PathSources, http.StatusOK},
		{"post sources", http.MethodPost, PathSources, http.StatusMethodNotAllowed},
		{"delete sources", http.MethodDelete, PathSources, http.StatusMethodNotAllowed},
		{"get healthz", http.MethodGet, PathHealthz, http.StatusOK},
		{"post healthz", http.MethodPost, PathHealthz, http.StatusMethodNotAllowed},
		{"unknown path", http.MethodGet, "/v1/unknown", http.StatusNotFound},
		{"root", http.MethodGet, "/", http.StatusNotFound},
		{"unversioned sources", http.MethodGet, "/sources", http.StatusNotFound},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := httptest.NewRecorder()
			h.ServeHTTP(rec, httptest.NewRequest(tt.method, tt.path, nil))
			assert.Equal(t, tt.want, rec.Code)
		})
	}
}

func TestHandler_HealthzBody(t *testing.T) {
	rec := httptest.NewRecorder()
	Handler(NewRegistry()).ServeHTTP(rec, httptest.NewRequest(http.MethodGet, PathHealthz, nil))
	assert.Equal(t, http.StatusOK, rec.Code)
	assert.Equal(t, "ok\n", rec.Body.String())
}
