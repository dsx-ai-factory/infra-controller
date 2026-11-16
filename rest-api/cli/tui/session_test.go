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
	"testing/synctest"
	"time"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// contextRoundTripFunc lets the fetch owner end a blocked request without a wall-clock wait.
type contextRoundTripFunc func(*http.Request) (*http.Response, error)

func (f contextRoundTripFunc) RoundTrip(request *http.Request) (*http.Response, error) {
	return f(request)
}

// TestSession_fetchRacks protects the registered fetch used by rack autocomplete.
func TestSession_fetchRacks(t *testing.T) {
	for _, tc := range []struct{ name string }{
		// The autocomplete deadline must reach HTTP rather than waiting for the shared client timeout.
		{name: "resolver honors autocomplete deadline"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				ctx, cancel := context.WithTimeout(context.Background(), autocompleteFetchTimeout)
				defer cancel()
				client := appcli.NewClient("https://api.example.com", "acme", "token", nil, false)
				requests := 0
				client.HTTPClient.Transport = contextRoundTripFunc(func(request *http.Request) (*http.Response, error) {
					requests++
					assert.Equal(t, "/v2/org/acme/nico/rack", request.URL.Path)
					assert.Equal(t, "site-1", request.URL.Query().Get("siteId"))
					<-request.Context().Done()
					return nil, request.Context().Err()
				})
				session := NewSession(client, "acme", "")
				session.Scope.SiteID = "site-1"
				started := time.Now()

				items, err := session.Resolver.Fetch(ctx, "rack")

				require.ErrorIs(t, err, context.DeadlineExceeded)
				assert.Nil(t, items)
				assert.Equal(t, 1, requests)
				assert.Equal(t, autocompleteFetchTimeout, time.Since(started))
				assert.Nil(t, session.Cache.Get("rack"), "a cancelled fetch must not populate the resolver cache")
			})
		})
	}
}

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

			items, err := session.fetchAll(context.Background(), apiPath(session, "site"), nil)

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
