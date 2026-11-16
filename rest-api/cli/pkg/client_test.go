// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
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
	statuses      []string
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
			statuses:      req.URL.Query()["status"],
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

// TestNewClient protects the default REST wait and an explicit caller timeout override.
func TestNewClient(t *testing.T) {
	for _, tc := range []struct {
		name    string
		timeout time.Duration
		wantErr bool
	}{
		// The default must leave room after the REST handler stops waiting for its workflow.
		{name: "default receives a response after the proxy wait"},
		// An embedding application still owns an intentionally shorter timeout.
		{name: "explicit shorter timeout", timeout: 20 * time.Second, wantErr: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				client := NewClient("https://api.example.com", "test-org", "test-token", nil, false)
				require.Greater(t, client.HTTPClient.Timeout, cutil.WorkflowContextTimeout)
				if tc.timeout != 0 {
					client.HTTPClient.Timeout = tc.timeout
				}
				client.HTTPClient.Transport = roundTripFunc(func(request *http.Request) (*http.Response, error) {
					select {
					case <-time.After(cutil.WorkflowContextTimeout + 5*time.Second):
						return &http.Response{StatusCode: http.StatusOK, Header: make(http.Header),
							Body: io.NopCloser(strings.NewReader(`{"id":"rack-01"}`))}, nil
					case <-request.Context().Done():
						return nil, request.Context().Err()
					}
				})

				body, _, err := client.Do(http.MethodGet, "/v2/org/{org}/nico/rack/{id}/power",
					map[string]string{"id": "rack-01"}, map[string]string{"siteId": "test-site"}, nil)

				if tc.wantErr {
					require.ErrorIs(t, err, context.DeadlineExceeded)
					return
				}
				require.NoError(t, err)
				assert.JSONEq(t, `{"id":"rack-01"}`, string(body))
			})
		})
	}
}

// TestClient_DoWithContext keeps cancellation and auth retries under the original owner deadline.
func TestClient_DoWithContext(t *testing.T) {
	for _, tc := range []struct {
		name          string
		phase         string
		wantRequests  int
		wantRefreshes int
		wantErr       error
	}{
		// A short owner deadline interrupts the first in-flight attempt.
		{name: "owner deadline", phase: "request", wantRequests: 1, wantErr: context.DeadlineExceeded},
		// An unauthorized response cannot start token refresh after its owner was cancelled.
		{name: "cancel before refresh", phase: "unauthorized", wantRequests: 1, wantErr: context.Canceled},
		// A non-context-aware refresh may finish after cancellation; it must not start another request.
		{name: "cancel during refresh", phase: "refresh", wantRequests: 1, wantRefreshes: 1, wantErr: context.Canceled},
		// Time spent refreshing belongs to the same deadline, not a fresh retry allowance.
		{name: "retry retains deadline", phase: "retry", wantRequests: 2, wantRefreshes: 1, wantErr: context.DeadlineExceeded},
		// The fallback must retain its owner even when the client's own timeout is disabled.
		{name: "owner cancels HTTP2 fallback", phase: "HTTP2"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if tc.phase == "HTTP2" {
				testClientDoWithContextCancelsFallback(t)
				return
			}
			synctest.Test(t, func(t *testing.T) {
				ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
				defer cancel()
				deadline, _ := ctx.Deadline()
				requests, refreshes := 0, 0
				client := NewClient("https://api.example.com", "test-org", "test-token", nil, false)
				client.HTTPClient.Transport = roundTripFunc(func(request *http.Request) (*http.Response, error) {
					requests++
					requestDeadline, _ := request.Context().Deadline()
					assert.Equal(t, deadline, requestDeadline)
					if requests == 1 && tc.phase != "request" {
						if tc.phase == "unauthorized" {
							cancel()
						}
						return &http.Response{StatusCode: http.StatusUnauthorized, Header: make(http.Header),
							Body: io.NopCloser(strings.NewReader(`{}`))}, nil
					}
					<-request.Context().Done()
					return nil, request.Context().Err()
				})
				client.TokenRefresh = func() (string, error) {
					refreshes++
					if tc.phase == "refresh" {
						cancel()
					} else {
						time.Sleep(time.Second)
					}
					return "refreshed-token", nil
				}
				started := time.Now()

				_, _, err := client.DoWithContext(ctx, http.MethodGet, "/v2/org/{org}/nico/rack/{id}/power",
					map[string]string{"id": "rack-01"}, nil, nil)

				require.ErrorIs(t, err, tc.wantErr)
				assert.Equal(t, tc.wantRequests, requests)
				assert.Equal(t, tc.wantRefreshes, refreshes)
				if tc.wantErr == context.DeadlineExceeded {
					assert.Equal(t, 2*time.Second, time.Since(started))
				}
			})
		})
	}
}

// testClientDoWithContextCancelsFallback cancels after HTTP/1.1 starts, so the
// initial HTTP/2 request cannot accidentally supply the cancellation proof.
func testClientDoWithContextCancelsFallback(t *testing.T) {
	t.Helper()

	fallbackStarted := make(chan struct{})
	server := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
		if request.ProtoMajor == 2 {
			w.WriteHeader(http.StatusOK)
			assert.NoError(t, http.NewResponseController(w).Flush())
			panic(http.ErrAbortHandler)
		}
		close(fallbackStarted)
		<-request.Context().Done()
	}))
	server.EnableHTTP2 = true
	server.StartTLS()
	defer server.Close()
	defer server.CloseClientConnections()
	transport := server.Client().Transport.(*http.Transport).Clone()
	transport.Protocols = new(http.Protocols)
	transport.Protocols.SetHTTP1(true)
	transport.Protocols.SetHTTP2(true)
	client := NewClient(server.URL, "test-org", "test-token", nil, false)
	client.HTTPClient.Transport = transport
	client.HTTPClient.Timeout = 0
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() {
		_, _, err := client.DoWithContext(ctx, http.MethodGet, "/v2/org/{org}/nico/rack/rack-01/power", nil, nil, nil)
		done <- err
	}()
	select {
	case <-fallbackStarted:
	case err := <-done:
		t.Fatalf("request returned before the HTTP/1.1 fallback started: %v", err)
	case <-time.After(5 * time.Second):
		t.Fatal("HTTP/1.1 fallback did not start")
	}

	cancel()

	select {
	case err := <-done:
		require.ErrorIs(t, err, context.Canceled)
	case <-time.After(5 * time.Second):
		t.Fatal("owner cancellation did not stop the HTTP/1.1 fallback")
	}
}

// TestClient_Do protects retries and authentication shared by CLI, TUI and MCP callers.
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
				require.Equal(t, observedRequest{protocol: 2, method: http.MethodGet, authorization: "Bearer test-token", pageSize: "100"}, <-requests)
				require.Equal(t, observedRequest{protocol: 1, method: http.MethodGet, authorization: "Bearer test-token", pageSize: "100"}, <-requests)
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
				require.Equal(t, observedRequest{protocol: 2, method: http.MethodPost, authorization: "Bearer test-token"}, <-requests)
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
		{name: "refreshes token on unauthorized and retries", run: testClientDoRefreshesTokenOnUnauthorizedAndRetries},
		{name: "retries unauthorized at most three times", run: testClientDoRetriesUnauthorizedAtMostThreeTimes},
		{name: "does not replay non-idempotent request after unauthorized", run: testClientDoDoesNotReplayNonIdempotentRequestAfterUnauthorized},
		{name: "returns refresh error without retrying", run: testClientDoReturnsRefreshErrorWithoutRetrying},
		{name: "returns empty token error without retrying", run: testClientDoReturnsEmptyTokenErrorWithoutRetrying},
		{name: "returns unauthorized without refresh function", run: testClientDoReturnsUnauthorizedWhenNoRefreshFunc},
		{name: "does not refresh on forbidden", run: testClientDoDoesNotRefreshOnForbidden},
	}

	for _, tt := range tests {
		t.Run(tt.name, tt.run)
	}
}

func TestClient_doWithQueryValues(t *testing.T) {
	tests := []struct {
		name string
		run  func(*testing.T)
	}{
		// Multi-value requests must preserve every value at the HTTP boundary.
		{
			name: "preserves repeated query values",
			run: func(t *testing.T) {
				var observed url.Values
				server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
					observed = req.URL.Query()
					w.WriteHeader(http.StatusNoContent)
				}))
				t.Cleanup(server.Close)

				client := NewClient(server.URL, "test-org", "test-token", nil, false)
				_, _, err := client.doWithQueryValues(
					http.MethodGet,
					"/v2/org/{org}/nico/vpc-peering",
					nil,
					url.Values{"status": {"Ready", "Error"}},
					nil,
				)

				require.NoError(t, err)
				assert.Equal(t, []string{"Ready", "Error"}, observed["status"])
			},
		},
		// The HTTP/1.1 fallback must replay the complete multi-value query.
		{
			name: "preserves repeated values across HTTP/2 retry",
			run: func(t *testing.T) {
				server, transport, requests := newHTTP2ResetServer(t)

				client := NewClient(server.URL, "test-org", "test-token", nil, false)
				client.HTTPClient.Transport = transport
				client.HTTPClient.Timeout = time.Second

				body, _, err := client.doWithQueryValues(
					http.MethodGet,
					"/v2/org/{org}/nico/vpc-peering",
					nil,
					url.Values{"status": {"Ready", "Error"}},
					nil,
				)

				require.NoError(t, err)
				require.JSONEq(t, `[{"id":"instance-1"}]`, string(body))
				require.Equal(t, observedRequest{
					protocol:      2,
					method:        http.MethodGet,
					authorization: "Bearer test-token",
					statuses:      []string{"Ready", "Error"},
				}, <-requests)
				require.Equal(t, observedRequest{
					protocol:      1,
					method:        http.MethodGet,
					authorization: "Bearer test-token",
					statuses:      []string{"Ready", "Error"},
				}, <-requests)
				require.Empty(t, requests)
			},
		},
		// Authorization refresh must retry with the same complete multi-value query.
		{
			name: "preserves repeated values across authorization retry",
			run: func(t *testing.T) {
				var requests []observedRequest
				server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
					requests = append(requests, observedRequest{
						method:        req.Method,
						authorization: req.Header.Get("Authorization"),
						statuses:      req.URL.Query()["status"],
					})
					if len(requests) == 1 {
						http.Error(w, `{"message":"expired"}`, http.StatusUnauthorized)
						return
					}
					w.WriteHeader(http.StatusNoContent)
				}))
				t.Cleanup(server.Close)

				client := NewClient(server.URL, "test-org", "stale-token", nil, false)
				client.TokenRefresh = func() (string, error) {
					return "refreshed-token", nil
				}

				_, _, err := client.doWithQueryValues(
					http.MethodGet,
					"/v2/org/{org}/nico/vpc-peering",
					nil,
					url.Values{"status": {"Ready", "Error"}},
					nil,
				)

				require.NoError(t, err)
				require.Equal(t, []observedRequest{
					{method: http.MethodGet, authorization: "Bearer stale-token", statuses: []string{"Ready", "Error"}},
					{method: http.MethodGet, authorization: "Bearer refreshed-token", statuses: []string{"Ready", "Error"}},
				}, requests)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, tt.run)
	}
}

func testClientDoRefreshesTokenOnUnauthorizedAndRetries(t *testing.T) {
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

func testClientDoRetriesUnauthorizedAtMostThreeTimes(t *testing.T) {
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

func testClientDoDoesNotReplayNonIdempotentRequestAfterUnauthorized(t *testing.T) {
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

func testClientDoReturnsRefreshErrorWithoutRetrying(t *testing.T) {
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

func testClientDoReturnsEmptyTokenErrorWithoutRetrying(t *testing.T) {
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

func testClientDoReturnsUnauthorizedWhenNoRefreshFunc(t *testing.T) {
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

func testClientDoDoesNotRefreshOnForbidden(t *testing.T) {
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
