// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestClient_DNSDomain drives each Domain call through the public client against
// an HTTP server, so the test sees the method, path, Site and body the API gets.
func TestClient_DNSDomain(t *testing.T) {
	const (
		basePath = "/v2/org/test-org/nico/domain/dns"
		domainID = "00000000-0000-0000-0000-00000000000a"
	)
	ttl := int32(600)
	domainJSON := map[string]any{
		"id": domainID, "siteId": "site-1", "name": "tenant-a.example.com",
		"defaultTtl": nil, "created": "2026-11-13T10:00:00Z", "updated": nil,
	}

	tests := []struct {
		name       string
		wantMethod string
		wantPath   string
		// wantSiteInQuery is false for create, which carries the Site in the body.
		wantSiteInQuery bool
		wantName        string
		wantQuery       map[string]string
		wantBody        map[string]any
		status          int
		respBody        any
		call            func(t *testing.T, client *Client)
	}{
		{
			name: "create sends the client's Site and omits an unset TTL", wantMethod: http.MethodPost, wantPath: basePath,
			wantBody: map[string]any{"siteId": "site-1", "name": "tenant-a.example.com"},
			status:   http.StatusCreated, respBody: domainJSON,
			call: func(t *testing.T, client *Client) {
				got, apiErr := client.CreateDNSDomain(context.Background(), DNSDomainCreateRequest{Name: "tenant-a.example.com"})
				require.Nil(t, apiErr)
				assert.Equal(t, domainID, got.ID)
				assert.Equal(t, "site-1", got.SiteID)
				assert.Nil(t, got.DefaultTTL, "a null TTL means the Site default")
				require.NotNil(t, got.Created)
				assert.Nil(t, got.Updated)
			},
		},
		{
			name: "create sends a TTL when set", wantMethod: http.MethodPost, wantPath: basePath,
			wantBody: map[string]any{"siteId": "site-1", "name": "tenant-a.example.com", "defaultTtl": float64(600)},
			status:   http.StatusCreated, respBody: domainJSON,
			call: func(t *testing.T, client *Client) {
				_, apiErr := client.CreateDNSDomain(context.Background(), DNSDomainCreateRequest{Name: "tenant-a.example.com", DefaultTTL: &ttl})
				require.Nil(t, apiErr)
			},
		},
		{
			name: "list returns the Domains and pagination", wantMethod: http.MethodGet, wantPath: basePath, wantSiteInQuery: true,
			status: http.StatusOK, respBody: []any{domainJSON},
			call: func(t *testing.T, client *Client) {
				got, page, apiErr := client.GetDNSDomains(context.Background(), nil, nil)
				require.Nil(t, apiErr)
				require.Len(t, got, 1)
				assert.Equal(t, "tenant-a.example.com", got[0].Name)
				require.NotNil(t, page)
				assert.Equal(t, 1, page.Total)
			},
		},
		{
			name: "list sends a name filter", wantMethod: http.MethodGet, wantPath: basePath, wantSiteInQuery: true,
			wantName: "tenant-a.example.com",
			status:   http.StatusOK, respBody: []any{domainJSON},
			call: func(t *testing.T, client *Client) {
				name := "tenant-a.example.com"
				got, _, apiErr := client.GetDNSDomains(context.Background(), &DNSDomainFilter{Name: &name}, nil)
				require.Nil(t, apiErr)
				require.Len(t, got, 1)
			},
		},
		{
			name: "list sends the page number, page size and order", wantMethod: http.MethodGet, wantPath: basePath, wantSiteInQuery: true,
			wantQuery: map[string]string{"pageNumber": "2", "pageSize": "5", "orderBy": "NAME_DESC"},
			status:    http.StatusOK, respBody: []any{domainJSON},
			call: func(t *testing.T, client *Client) {
				pageNumber, pageSize, orderBy := 2, 5, "NAME_DESC"
				_, _, apiErr := client.GetDNSDomains(context.Background(), nil,
					&PaginationFilter{PageNumber: &pageNumber, PageSize: &pageSize, OrderBy: &orderBy})
				require.Nil(t, apiErr)
			},
		},
		{
			name: "create returns the API error", wantMethod: http.MethodPost, wantPath: basePath,
			status: http.StatusBadRequest, respBody: map[string]any{"message": "invalid Domain name"},
			call: func(t *testing.T, client *Client) {
				got, apiErr := client.CreateDNSDomain(context.Background(), DNSDomainCreateRequest{Name: "tenant-a.example.com"})
				assert.Nil(t, got)
				require.NotNil(t, apiErr)
				assert.Equal(t, http.StatusBadRequest, apiErr.Code)
				assert.Equal(t, "invalid Domain name", apiErr.Message)
			},
		},
		{
			name: "list returns the API error", wantMethod: http.MethodGet, wantPath: basePath, wantSiteInQuery: true,
			status: http.StatusForbidden, respBody: map[string]any{"message": "not a Provider Admin"},
			call: func(t *testing.T, client *Client) {
				got, page, apiErr := client.GetDNSDomains(context.Background(), nil, nil)
				assert.Nil(t, got)
				assert.Nil(t, page)
				require.NotNil(t, apiErr)
				assert.Equal(t, http.StatusForbidden, apiErr.Code)
				assert.Equal(t, "not a Provider Admin", apiErr.Message)
			},
		},
		{
			name: "get of an unknown Domain returns the API error", wantMethod: http.MethodGet, wantPath: basePath + "/" + domainID, wantSiteInQuery: true,
			status: http.StatusNotFound, respBody: map[string]any{"message": "Domain not found"},
			call: func(t *testing.T, client *Client) {
				got, apiErr := client.GetDNSDomain(context.Background(), domainID)
				assert.Nil(t, got)
				require.NotNil(t, apiErr)
				assert.Equal(t, http.StatusNotFound, apiErr.Code)
				assert.Equal(t, "Domain not found", apiErr.Message)
			},
		},
		{
			name: "update returns the API error", wantMethod: http.MethodPatch, wantPath: basePath + "/" + domainID,
			status: http.StatusBadRequest, respBody: map[string]any{"message": "defaultTtl is out of range"},
			call: func(t *testing.T, client *Client) {
				got, apiErr := client.UpdateDNSDomain(context.Background(), domainID, DNSDomainUpdateRequest{DefaultTTL: 5})
				assert.Nil(t, got)
				require.NotNil(t, apiErr)
				assert.Equal(t, http.StatusBadRequest, apiErr.Code)
				assert.Equal(t, "defaultTtl is out of range", apiErr.Message)
			},
		},
		{
			name: "update sends the client's Site and the new TTL", wantMethod: http.MethodPatch, wantPath: basePath + "/" + domainID,
			wantBody: map[string]any{"siteId": "site-1", "defaultTtl": float64(900)},
			status:   http.StatusOK, respBody: map[string]any{
				"id": domainID, "siteId": "site-1", "name": "tenant-a.example.com",
				"defaultTtl": 900, "created": "2026-11-13T10:00:00Z", "updated": "2026-11-13T11:00:00Z",
			},
			call: func(t *testing.T, client *Client) {
				got, apiErr := client.UpdateDNSDomain(context.Background(), domainID, DNSDomainUpdateRequest{DefaultTTL: 900})
				require.Nil(t, apiErr)
				require.NotNil(t, got.DefaultTTL)
				assert.Equal(t, int32(900), *got.DefaultTTL)
			},
		},
		{
			name: "get returns one Domain", wantMethod: http.MethodGet, wantPath: basePath + "/" + domainID, wantSiteInQuery: true,
			status: http.StatusOK, respBody: domainJSON,
			call: func(t *testing.T, client *Client) {
				got, apiErr := client.GetDNSDomain(context.Background(), domainID)
				require.Nil(t, apiErr)
				assert.Equal(t, domainID, got.ID)
			},
		},
		{
			name: "delete succeeds with no content", wantMethod: http.MethodDelete, wantPath: basePath + "/" + domainID, wantSiteInQuery: true,
			status: http.StatusNoContent,
			call: func(t *testing.T, client *Client) {
				require.Nil(t, client.DeleteDNSDomain(context.Background(), domainID))
			},
		},
		{
			name: "delete of a Domain still in use returns the API error", wantMethod: http.MethodDelete, wantPath: basePath + "/" + domainID, wantSiteInQuery: true,
			status: http.StatusPreconditionFailed, respBody: map[string]any{"message": "domain is still referenced"},
			call: func(t *testing.T, client *Client) {
				apiErr := client.DeleteDNSDomain(context.Background(), domainID)
				require.NotNil(t, apiErr)
				assert.Equal(t, http.StatusPreconditionFailed, apiErr.Code)
				assert.Equal(t, "domain is still referenced", apiErr.Message)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			called := false
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				called = true
				assert.Equal(t, tt.wantMethod, r.Method)
				assert.Equal(t, tt.wantPath, r.URL.Path)
				if tt.wantSiteInQuery {
					assert.Equal(t, "site-1", r.URL.Query().Get("siteId"))
				}
				assert.Equal(t, tt.wantName, r.URL.Query().Get("name"))
				for key, want := range tt.wantQuery {
					assert.Equal(t, want, r.URL.Query().Get(key), key)
				}
				if tt.wantBody != nil {
					var body map[string]any
					if assert.NoError(t, json.NewDecoder(r.Body).Decode(&body)) {
						assert.Equal(t, tt.wantBody, body)
					}
				}
				w.Header().Set("Content-Type", "application/json")
				w.Header().Set("X-Pagination", `{"pageNumber":1,"pageSize":20,"total":1}`)
				w.WriteHeader(tt.status)
				if tt.respBody != nil {
					assert.NoError(t, json.NewEncoder(w).Encode(tt.respBody))
				}
			}))
			defer server.Close()

			tt.call(t, newSimpleTestClient(server.URL))
			assert.True(t, called, "the call must reach the API")
		})
	}
}

// TestClient_DNSDomain_BrokenSuccessResponse covers a response that starts with a
// success status and then has no usable body. Each call must return an error
// instead of a result, and must not panic.
func TestClient_DNSDomain_BrokenSuccessResponse(t *testing.T) {
	const domainID = "00000000-0000-0000-0000-00000000000a"

	responses := []struct {
		name        string
		write       func(w http.ResponseWriter)
		wantMessage string
	}{
		{
			// The declared length is longer than the body, so the server
			// closes the connection and the client's read fails.
			name: "body cut short",
			write: func(w http.ResponseWriter) {
				w.Header().Set("Content-Length", "512")
				w.WriteHeader(http.StatusOK)
				_, _ = w.Write([]byte(`{"id":`))
			},
			wantMessage: "Error processing API response: ",
		},
		{
			name: "body is not valid JSON",
			write: func(w http.ResponseWriter) {
				w.WriteHeader(http.StatusOK)
				_, _ = w.Write([]byte(`{"id":`))
			},
			wantMessage: "Error processing API response: ",
		},
	}
	calls := []struct {
		name string
		call func(client *Client) *ApiError
	}{
		{name: "create", call: func(client *Client) *ApiError {
			got, apiErr := client.CreateDNSDomain(context.Background(), DNSDomainCreateRequest{Name: "tenant-a.example.com"})
			assert.Nil(t, got)
			return apiErr
		}},
		{name: "list", call: func(client *Client) *ApiError {
			got, pagination, apiErr := client.GetDNSDomains(context.Background(), nil, nil)
			assert.Nil(t, got)
			assert.Nil(t, pagination)
			return apiErr
		}},
		{name: "get", call: func(client *Client) *ApiError {
			got, apiErr := client.GetDNSDomain(context.Background(), domainID)
			assert.Nil(t, got)
			return apiErr
		}},
		{name: "update", call: func(client *Client) *ApiError {
			got, apiErr := client.UpdateDNSDomain(context.Background(), domainID, DNSDomainUpdateRequest{DefaultTTL: 900})
			assert.Nil(t, got)
			return apiErr
		}},
	}

	for _, response := range responses {
		for _, c := range calls {
			t.Run(c.name+" "+response.name, func(t *testing.T) {
				server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
					w.Header().Set("Content-Type", "application/json")
					response.write(w)
				}))
				defer server.Close()

				apiErr := c.call(newSimpleTestClient(server.URL))
				require.NotNil(t, apiErr)
				assert.Equal(t, http.StatusInternalServerError, apiErr.Code)
				assert.Contains(t, apiErr.Message, response.wantMessage)
			})
		}
	}

	// A JSON null decodes without error and leaves no Domain to return.
	for _, c := range calls {
		if c.name == "list" {
			continue
		}
		t.Run(c.name+" null body", func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(http.StatusOK)
				_, _ = w.Write([]byte("null"))
			}))
			defer server.Close()

			apiErr := c.call(newSimpleTestClient(server.URL))
			require.NotNil(t, apiErr)
			assert.Equal(t, http.StatusInternalServerError, apiErr.Code)
			assert.Equal(t, "Domain API returned an empty response", apiErr.Message)
		})
	}
}
