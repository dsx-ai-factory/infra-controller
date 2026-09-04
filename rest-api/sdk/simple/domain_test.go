// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestDomainManagerCRUD(t *testing.T) {
	const domainJSON = `{"id":"domain-1","name":"tenant.example.com","siteId":"site-1","tenantId":"tenant-1","created":"2026-09-02T12:00:00Z","updated":"2026-09-02T12:00:00Z"}`
	requests := make([]string, 0, 5)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		requests = append(requests, r.Method+" "+r.URL.RequestURI())
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/v2/org/test-org/nico/domain":
			var body map[string]any
			require.NoError(t, json.NewDecoder(r.Body).Decode(&body))
			assert.Equal(t, "tenant.example.com", body["name"])
			assert.Equal(t, "site-1", body["siteId"])
			w.WriteHeader(http.StatusCreated)
			_, _ = io.WriteString(w, domainJSON)
		case r.Method == http.MethodGet && r.URL.Path == "/v2/org/test-org/nico/domain":
			assert.Equal(t, "site-1", r.URL.Query().Get("siteId"))
			assert.Equal(t, "tenant-1", r.URL.Query().Get("tenantId"))
			_, _ = io.WriteString(w, "["+domainJSON+"]")
		case r.Method == http.MethodGet && r.URL.Path == "/v2/org/test-org/nico/domain/domain-1":
			_, _ = io.WriteString(w, domainJSON)
		case r.Method == http.MethodPatch && r.URL.Path == "/v2/org/test-org/nico/domain/domain-1":
			var body map[string]any
			require.NoError(t, json.NewDecoder(r.Body).Decode(&body))
			assert.Equal(t, "renamed.example.com", body["name"])
			_, _ = io.WriteString(w, domainJSON)
		case r.Method == http.MethodDelete && r.URL.Path == "/v2/org/test-org/nico/domain/domain-1":
			w.WriteHeader(http.StatusNoContent)
		default:
			t.Errorf("unexpected request: %s %s", r.Method, r.URL.RequestURI())
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	client := newSimpleTestClient(server.URL)
	manager := NewDomainManager(client)
	ctx := context.Background()

	created, apiErr := manager.Create(ctx, DomainCreateRequest{Name: "tenant.example.com"})
	require.Nil(t, apiErr)
	require.NotNil(t, created)
	assert.Equal(t, "domain-1", created.ID)
	assert.Equal(t, "site-1", created.SiteID)
	assert.Equal(t, "tenant-1", created.TenantID)
	assert.Equal(t, time.Date(2026, 9, 2, 12, 0, 0, 0, time.UTC), created.Created)

	siteID := "site-1"
	tenantID := "tenant-1"
	domains, apiErr := manager.GetDomains(ctx, &DomainFilter{SiteID: &siteID, TenantID: &tenantID})
	require.Nil(t, apiErr)
	require.Len(t, domains, 1)
	assert.Equal(t, "tenant.example.com", domains[0].Name)

	domain, apiErr := manager.Get(ctx, "domain-1")
	require.Nil(t, apiErr)
	require.NotNil(t, domain)
	assert.Equal(t, "domain-1", domain.ID)

	updated, apiErr := manager.Update(ctx, "domain-1", DomainUpdateRequest{Name: "renamed.example.com"})
	require.Nil(t, apiErr)
	require.NotNil(t, updated)
	assert.Equal(t, "domain-1", updated.ID)

	require.Nil(t, manager.Delete(ctx, "domain-1"))
	assert.Equal(t, []string{
		"POST /v2/org/test-org/nico/domain",
		"GET /v2/org/test-org/nico/domain?pageNumber=1&siteId=site-1&tenantId=tenant-1",
		"GET /v2/org/test-org/nico/domain/domain-1",
		"PATCH /v2/org/test-org/nico/domain/domain-1",
		"DELETE /v2/org/test-org/nico/domain/domain-1",
	}, requests)
}
