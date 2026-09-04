// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strconv"
	"sync/atomic"
	"testing"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSessionFetchDomainsUsesSiteScopeAndCache(t *testing.T) {
	var domainRequests atomic.Int32
	var tenantRequests atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		assert.Equal(t, http.MethodGet, r.Method)
		switch r.URL.Path {
		case "/v2/org/acme/nico/tenant/current":
			tenantRequests.Add(1)
			_, err := io.WriteString(w, `{"id":"tenant-1"}`)
			require.NoError(t, err)
		case "/v2/org/acme/nico/domain":
			domainRequests.Add(1)
			assert.Equal(t, "site-1", r.URL.Query().Get("siteId"))
			assert.Equal(t, "tenant-1", r.URL.Query().Get("tenantId"))
			assert.Equal(t, "100", r.URL.Query().Get("pageSize"))
			w.Header().Set("X-Pagination", `{"total":101}`)
			switch page := r.URL.Query().Get("pageNumber"); page {
			case "1":
				domains := make([]map[string]string, 100)
				for i := range domains {
					id := "domain-" + strconv.Itoa(i+1)
					domains[i] = map[string]string{"id": id, "name": id + ".example.com", "siteId": "site-1", "tenantId": "tenant-1"}
				}
				require.NoError(t, json.NewEncoder(w).Encode(domains))
			case "2":
				_, err := io.WriteString(w, `[{"id":"domain-101","name":"domain-101.example.com","siteId":"site-1","tenantId":"tenant-1"}]`)
				require.NoError(t, err)
			default:
				t.Errorf("unexpected Domain page %q", page)
			}
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
	session.Scope.SiteID = "site-1"

	for range 2 {
		domains, err := session.Resolver.Fetch(context.Background(), "domain")
		require.NoError(t, err)
		require.Len(t, domains, 101)
		assert.Equal(t, "domain-1", domains[0].ID)
		assert.Equal(t, "domain-1.example.com", domains[0].Name)
		assert.Equal(t, "site-1", domains[0].Extra["siteId"])
		assert.Equal(t, "tenant-1", domains[0].Extra["tenantId"])
		assert.Equal(t, "domain-101", domains[100].ID)
	}
	assert.EqualValues(t, 1, tenantRequests.Load())
	assert.EqualValues(t, 2, domainRequests.Load())
}

func TestSessionFetchDomainsRejectsRowsOutsideCurrentTenant(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		assert.Equal(t, "/v2/org/acme/nico/domain", r.URL.Path)
		assert.Equal(t, "tenant-1", r.URL.Query().Get("tenantId"))
		_, err := io.WriteString(w, `[
			{"id":"domain-1","name":"tenant.example.com","siteId":"site-1","tenantId":"tenant-1"},
			{"id":"domain-other","name":"other.example.com","siteId":"site-1","tenantId":"tenant-2"},
			{"id":"domain-provider","name":"provider.example.com","siteId":"site-1","tenantId":null}
		]`)
		require.NoError(t, err)
	}))
	defer server.Close()

	session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
	session.Cache.Set("_tenant", []NamedItem{{Name: "acme", ID: "tenant-1"}})

	_, err := session.Resolver.Fetch(context.Background(), "domain")

	require.EqualError(t, err, `domain "domain-other" is not owned by the current tenant`)
}
