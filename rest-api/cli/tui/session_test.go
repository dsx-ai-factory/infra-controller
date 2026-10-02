// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSession_fetchAll(t *testing.T) {
	firstPage := make([]map[string]string, 100)
	for i := range firstPage {
		firstPage[i] = map[string]string{"id": fmt.Sprintf("site-%d", i)}
	}
	firstBody, err := json.Marshal(firstPage)
	require.NoError(t, err)

	for _, test := range []struct {
		name          string
		pages         []string
		wantError     string
		wantErrorType any
	}{
		{
			name:          "malformed first page",
			pages:         []string{"["},
			wantError:     "parsing /v2/org/{org}/nico/site page 1:",
			wantErrorType: new(*json.SyntaxError),
		},
		{
			name:          "invalid later page",
			pages:         []string{string(firstBody), `{}`},
			wantError:     "parsing /v2/org/{org}/nico/site page 2:",
			wantErrorType: new(*json.UnmarshalTypeError),
		},
		{
			name:  "empty list",
			pages: []string{`[]`},
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			requests := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests++
				assert.Equal(t, "/v2/org/acme/nico/site", r.URL.Path)
				assert.Equal(t, fmt.Sprint(requests), r.URL.Query().Get("pageNumber"))
				assert.Equal(t, "100", r.URL.Query().Get("pageSize"))
				if !assert.LessOrEqual(t, requests, len(test.pages)) {
					http.Error(w, "unexpected page", http.StatusInternalServerError)
					return
				}
				_, err := io.WriteString(w, test.pages[requests-1])
				assert.NoError(t, err)
			}))
			defer server.Close()
			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")

			items, err := session.fetchAll(apiPath(session, "site"), nil)

			assert.Equal(t, len(test.pages), requests)
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				assert.Nil(t, items)
				assert.ErrorAs(t, err, test.wantErrorType)
				return
			}
			require.NoError(t, err)
			assert.Empty(t, items)
		})
	}
}

// The Domain list includes reservations, but the subnet chooser must only
// offer a Domain once its Core-backed ownership index has reached Ready.
func TestSessionDomainResolver_OnlyReadyDomainSelectable(t *testing.T) {
	status := "Pending"
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/v2/org/acme/nico/domain":
			assert.Equal(t, "tenant-1", r.URL.Query().Get("tenantId"))
			assert.Equal(t, "site-1", r.URL.Query().Get("siteId"))
			_, _ = fmt.Fprintf(w, `[{"id":"domain-1","name":"dev.example","status":%q,"tenantId":"tenant-1","siteId":"site-1"},{"id":"domain-2","name":"deleting.example","status":"Deleting","tenantId":"tenant-1","siteId":"site-1"}]`, status)
		default:
			http.Error(w, "unexpected path", http.StatusNotFound)
		}
	}))
	defer server.Close()

	session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
	session.Scope.SiteID = "site-1"
	session.Cache.Set("_tenant", []NamedItem{{Name: "acme", ID: "tenant-1"}})

	items, err := session.Resolver.Fetch(context.Background(), "domain")
	require.NoError(t, err)
	assert.Empty(t, items)
	_, err = session.Resolver.Resolve(context.Background(), "domain", "DNS Domain")
	require.ErrorContains(t, err, "no DNS Domain available")

	status = "Ready"
	session.Cache.Invalidate("domain")
	item, err := session.Resolver.Resolve(context.Background(), "domain", "DNS Domain")
	require.NoError(t, err)
	assert.Equal(t, "domain-1", item.ID)
	assert.Equal(t, "Ready", item.Status)
	assert.Equal(t, "site-1", item.Extra["siteId"])
}
