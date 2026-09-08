// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

type roundTripFunc func(*http.Request) (*http.Response, error)

func (rtf roundTripFunc) RoundTrip(req *http.Request) (*http.Response, error) {
	return rtf(req)
}

type errorReadCloser struct {
	err    error
	closed bool
}

func (erc *errorReadCloser) Read([]byte) (int, error) {
	return 0, erc.err
}

func (erc *errorReadCloser) Close() error {
	erc.closed = true
	return nil
}

type observedRequest struct {
	protocol      int
	method        string
	authorization string
	pageSize      string
}

func newHTTP2ResetServer(t *testing.T) (*httptest.Server, *http.Transport, <-chan observedRequest) {
	t.Helper()

	requests := make(chan observedRequest, 2)
	server := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		requests <- observedRequest{
			protocol:      req.ProtoMajor,
			method:        req.Method,
			authorization: req.Header.Get("Authorization"),
			pageSize:      req.URL.Query().Get("pageSize"),
		}

		if req.ProtoMajor == 2 {
			w.WriteHeader(http.StatusOK)
			assert.NoError(t, http.NewResponseController(w).Flush())
			panic(http.ErrAbortHandler)
		}

		_, err := w.Write([]byte(`[{"id":"instance-1"}]`))
		assert.NoError(t, err)
	}))
	server.EnableHTTP2 = true
	server.StartTLS()
	t.Cleanup(server.Close)

	transport := server.Client().Transport.(*http.Transport).Clone()
	transport.Protocols = new(http.Protocols)
	transport.Protocols.SetHTTP1(true)
	transport.Protocols.SetHTTP2(true)

	return server, transport, requests
}

func TestClient_Do(t *testing.T) {
	tests := []struct {
		name string
		run  func(*testing.T)
	}{
		{
			name: "retries an HTTP/2 INTERNAL_ERROR once over HTTP/1.1",
			run: func(t *testing.T) {
				server, transport, requests := newHTTP2ResetServer(t)

				client := NewClient(server.URL, "test-org", "test-token", nil, false)
				client.HTTPClient.Transport = transport
				client.HTTPClient.Timeout = time.Second

				body, _, err := client.Do(
					http.MethodGet,
					"/v2/org/{org}/nico/instance",
					nil,
					map[string]string{"pageSize": "100"},
					nil,
				)

				require.NoError(t, err)
				require.JSONEq(t, `[{"id":"instance-1"}]`, string(body))
				require.Equal(t, observedRequest{2, http.MethodGet, "Bearer test-token", "100"}, <-requests)
				require.Equal(t, observedRequest{1, http.MethodGet, "Bearer test-token", "100"}, <-requests)
				require.Empty(t, requests)
			},
		},
		{
			name: "does not retry a mutation",
			run: func(t *testing.T) {
				server, transport, requests := newHTTP2ResetServer(t)

				client := NewClient(server.URL, "test-org", "test-token", nil, false)
				client.HTTPClient.Transport = transport

				_, _, err := client.Do(
					http.MethodPost,
					"/v2/org/{org}/nico/instance",
					nil,
					nil,
					[]byte(`{"name":"instance-1"}`),
				)

				require.ErrorContains(t, err, "INTERNAL_ERROR")
				require.Equal(t, observedRequest{2, http.MethodPost, "Bearer test-token", ""}, <-requests)
				require.Empty(t, requests)
			},
		},
		{
			name: "does not retry a non-HTTP/2 read error",
			run: func(t *testing.T) {
				body := &errorReadCloser{err: errors.New("non-HTTP/2 read failure")}
				requests := 0
				client := NewClient("https://api.example.com", "test-org", "test-token", nil, false)
				client.HTTPClient.Transport = roundTripFunc(func(*http.Request) (*http.Response, error) {
					requests++
					return &http.Response{
						StatusCode: http.StatusOK,
						Header:     make(http.Header),
						Body:       body,
					}, nil
				})

				_, _, err := client.Do(
					http.MethodGet,
					"/v2/org/{org}/nico/instance",
					nil,
					nil,
					nil,
				)

				require.ErrorContains(t, err, "non-HTTP/2 read failure")
				require.Equal(t, 1, requests)
				require.True(t, body.closed)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, tt.run)
	}
}

func TestClientDoRefreshesTokenOnUnauthorizedAndRetries(t *testing.T) {
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests++
		if requests == 1 {
			http.Error(w, `{"message":"expired"}`, http.StatusUnauthorized)
			return
		}
		if got := r.Header.Get("Authorization"); got != "Bearer refreshed-token" {
			require.Equal(t, "Bearer refreshed-token", got)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"ok":true}`))
	}))
	defer server.Close()

	refreshes := 0
	client := NewClient(server.URL, "test-org", "stale-token", nil, false)
	client.TokenRefresh = func() (string, error) {
		refreshes++
		return "refreshed-token", nil
	}

	body, _, err := client.Do("GET", "/v2/org/{org}/nico/test", nil, nil, nil)
	require.NoError(t, err)
	require.Equal(t, `{"ok":true}`, string(body))
	require.Equal(t, 2, requests)
	require.Equal(t, 1, refreshes)
}

func TestClientDoRetriesUnauthorizedAtMostThreeTimes(t *testing.T) {
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		requests++
		http.Error(w, `{"message":"expired"}`, http.StatusUnauthorized)
	}))
	defer server.Close()

	var events []AuthRetryEvent
	refreshes := 0
	client := NewClient(server.URL, "test-org", "stale-token", nil, false)
	client.TokenRefresh = func() (string, error) {
		refreshes++
		return "still-invalid-token", nil
	}
	client.AuthRetryNotify = func(event AuthRetryEvent) {
		events = append(events, event)
	}

	_, _, err := client.Do("GET", "/v2/org/{org}/nico/test", nil, nil, nil)
	apiErr, ok := err.(*APIError)
	require.True(t, ok, "err = %T, want *APIError", err)
	require.Equal(t, http.StatusUnauthorized, apiErr.StatusCode)
	require.Equal(t, 4, requests)
	require.Equal(t, 3, refreshes)
	require.Len(t, events, 6)
	for i := 0; i < 3; i++ {
		login := events[i*2]
		retry := events[i*2+1]
		require.Equal(t, AuthRetryActionLogin, login.Action)
		require.Equal(t, AuthRetryActionRetry, retry.Action)
		require.Equal(t, i+1, login.Attempt)
		require.Equal(t, i+1, retry.Attempt)
		require.Equal(t, 3, login.MaxAttempts)
		require.Equal(t, 3, retry.MaxAttempts)
	}
}

func TestClientDoDoesNotReplayNonIdempotentRequestAfterUnauthorized(t *testing.T) {
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		requests++
		http.Error(w, `{"message":"expired"}`, http.StatusUnauthorized)
	}))
	defer server.Close()

	var events []AuthRetryEvent
	refreshes := 0
	client := NewClient(server.URL, "test-org", "stale-token", nil, false)
	client.TokenRefresh = func() (string, error) {
		refreshes++
		return "new-token", nil
	}
	client.AuthRetryNotify = func(event AuthRetryEvent) {
		events = append(events, event)
	}

	_, _, err := client.Do("POST", "/v2/org/{org}/nico/test", nil, nil, []byte(`{"name":"x"}`))
	apiErr, ok := err.(*APIError)
	require.True(t, ok, "err = %T, want *APIError", err)
	require.Equal(t, http.StatusUnauthorized, apiErr.StatusCode)
	require.Equal(t, 1, requests)
	require.Equal(t, 0, refreshes)
	require.Len(t, events, 1)
	require.Equal(t, AuthRetryActionSkip, events[0].Action)
	require.Equal(t, http.MethodPost, events[0].Method)
}

func TestClientDoReturnsRefreshErrorWithoutRetrying(t *testing.T) {
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		requests++
		http.Error(w, `{"message":"expired"}`, http.StatusUnauthorized)
	}))
	defer server.Close()

	refreshes := 0
	client := NewClient(server.URL, "test-org", "stale-token", nil, false)
	client.TokenRefresh = func() (string, error) {
		refreshes++
		return "", errors.New("refresh failed")
	}

	_, _, err := client.Do("GET", "/v2/org/{org}/nico/test", nil, nil, nil)
	require.Error(t, err)
	require.True(t, strings.Contains(err.Error(), "refresh failed"), "err = %v", err)
	require.Equal(t, 1, requests)
	require.Equal(t, 1, refreshes)
}

func TestClientDoReturnsEmptyTokenErrorWithoutRetrying(t *testing.T) {
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		requests++
		http.Error(w, `{"message":"expired"}`, http.StatusUnauthorized)
	}))
	defer server.Close()

	refreshes := 0
	client := NewClient(server.URL, "test-org", "stale-token", nil, false)
	client.TokenRefresh = func() (string, error) {
		refreshes++
		return "", nil
	}

	_, _, err := client.Do("GET", "/v2/org/{org}/nico/test", nil, nil, nil)
	require.Error(t, err)
	require.True(t, strings.Contains(err.Error(), "no token returned"), "err = %v", err)
	require.Equal(t, 1, requests)
	require.Equal(t, 1, refreshes)
}

func TestClientDoReturnsUnauthorizedWhenNoRefreshFunc(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, `{"message":"expired"}`, http.StatusUnauthorized)
	}))
	defer server.Close()

	client := NewClient(server.URL, "test-org", "stale-token", nil, false)
	_, _, err := client.Do("GET", "/v2/org/{org}/nico/test", nil, nil, nil)
	apiErr, ok := err.(*APIError)
	require.True(t, ok, "err = %T, want *APIError", err)
	require.Equal(t, http.StatusUnauthorized, apiErr.StatusCode)
}

func TestClientDoDoesNotRefreshOnForbidden(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, `{"message":"forbidden"}`, http.StatusForbidden)
	}))
	defer server.Close()

	refreshes := 0
	client := NewClient(server.URL, "test-org", "token", nil, false)
	client.TokenRefresh = func() (string, error) {
		refreshes++
		return "new-token", nil
	}

	_, _, err := client.Do("GET", "/v2/org/{org}/nico/test", nil, nil, nil)
	apiErr, ok := err.(*APIError)
	require.True(t, ok, "err = %T, want *APIError", err)
	require.Equal(t, http.StatusForbidden, apiErr.StatusCode)
	require.Equal(t, 0, refreshes)
}

func TestFormatDebugBodyRedactsNestedSecrets(t *testing.T) {
	body := []byte(`{
		"tokenEndpoint":"https://issuer.example/token",
		"clientSecretBasic":{"clientId":"client-one","clientSecret":"secret-value"},
		"credentials":[{"password":"password-value"}],
		"secret":{"value":"nested-secret-value"},
		"databaseCredentials":[{"value":"nested-credential-value"}],
		"accessToken":"access-token-value",
		"publicKey":"ssh-ed25519 public"
	}`)

	formatted := formatDebugBody(body)

	assert.NotContains(t, formatted, "secret-value")
	assert.NotContains(t, formatted, "password-value")
	assert.NotContains(t, formatted, "nested-secret-value")
	assert.NotContains(t, formatted, "nested-credential-value")
	assert.NotContains(t, formatted, "access-token-value")
	assert.Contains(t, formatted, `"tokenEndpoint":"https://issuer.example/token"`)
	assert.Contains(t, formatted, `"clientId":"client-one"`)
	assert.Contains(t, formatted, `"clientSecret":"<redacted>"`)
	assert.Contains(t, formatted, `"credentials":[{"password":"<redacted>"}]`)
	assert.Contains(t, formatted, `"secret":{"value":"<redacted>"}`)
	assert.Contains(t, formatted, `"databaseCredentials":[{"value":"<redacted>"}]`)
	assert.Contains(t, formatted, `"accessToken":"<redacted>"`)
	assert.Contains(t, formatted, `"publicKey":"ssh-ed25519 public"`)
}

func TestFormatDebugBodyPreservesJSONNumberLiterals(t *testing.T) {
	assert.Equal(
		t,
		`{"epoch":1753200000000,"id":9007199254740993}`,
		formatDebugBody([]byte(`{"epoch":1753200000000,"id":9007199254740993}`)),
	)
}

func TestFormatDebugBodyPreservesNonJSONDiagnostics(t *testing.T) {
	assert.Equal(t, "upstream unavailable", formatDebugBody([]byte("upstream unavailable")))
}
