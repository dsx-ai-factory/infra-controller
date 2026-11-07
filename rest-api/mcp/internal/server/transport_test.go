// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/stretchr/testify/require"
)

// synthSpec is a minimal three-operation OpenAPI document that drives
// the integration tests. The org path param is required because that's
// the real shape of NICo REST routes (/v2/org/{org}/...).
const synthSpec = `
openapi: 3.0.0
info:
  title: SynthNICo
  version: 0.0.1
paths:
  /v2/org/{org}/nico/foo:
    parameters:
      - {name: org, in: path, required: true, schema: {type: string}}
    get:
      operationId: get-all-foo
      summary: List foos
      parameters:
        - {name: pageSize, in: query, schema: {type: integer}}
  /v2/org/{org}/nico/foo/{fooId}:
    parameters:
      - {name: org, in: path, required: true, schema: {type: string}}
      - {name: fooId, in: path, required: true, schema: {type: string}}
    get:
      operationId: get-foo
      summary: Retrieve a foo
  /v2/org/{org}/nico/foo/{fooId}/status-history:
    parameters:
      - {name: org, in: path, required: true, schema: {type: string}}
      - {name: fooId, in: path, required: true, schema: {type: string}}
    get:
      operationId: get-foo-status-history
      summary: Foo status history
`

func TestHandler_RejectsLongPollGET(t *testing.T) {
	server, err := BuildServer([]byte(synthSpec), Options{BaseURL: "http://example.test", Org: "x"})
	require.NoError(t, err)
	ts := httptest.NewServer(NewHandler(server))
	defer ts.Close()

	req, err := http.NewRequest(http.MethodGet, ts.URL, nil)
	require.NoError(t, err)
	req.Header.Set("Accept", "text/event-stream")
	resp, err := http.DefaultClient.Do(req)
	require.NoError(t, err)
	defer resp.Body.Close()

	// In Stateless mode the SDK rejects long-poll GETs because there is
	// no session for the server to push notifications onto. The exact
	// status code is an SDK choice; we assert it is a non-success
	// response so a future SDK change is caught.
	require.GreaterOrEqual(t, resp.StatusCode, http.StatusBadRequest,
		"long-poll GET on /mcp must be rejected in stateless mode (got %d)", resp.StatusCode)
}

func TestHandler_ToolsListAndJSONResponse(t *testing.T) {
	server, err := BuildServer([]byte(synthSpec), Options{BaseURL: "http://example.test", Org: "x"})
	require.NoError(t, err)
	ts := httptest.NewServer(NewHandler(server))
	defer ts.Close()

	resp := mcpPost(t, ts.URL, "", jsonrpcRequest(1, "tools/list", map[string]any{}))
	defer resp.Body.Close()

	require.Equal(t, http.StatusOK, resp.StatusCode)
	ctype := resp.Header.Get("Content-Type")
	require.True(t, strings.HasPrefix(ctype, "application/json"),
		"response Content-Type must be application/json, never text/event-stream (got %q)", ctype)

	body, err := io.ReadAll(resp.Body)
	require.NoError(t, err)
	tools := decodeToolList(t, body)

	wantNames := []string{
		"nico_get_all_foo",
		"nico_get_foo",
		"nico_get_foo_status_history",
	}
	gotNames := make([]string, 0, len(tools))
	for _, tool := range tools {
		gotNames = append(gotNames, tool.Name)
	}
	require.ElementsMatch(t, wantNames, gotNames)
}

func TestHandler_ToolsCall_RejectsOutOfRangeParam(t *testing.T) {
	specYAML := strings.Replace(synthSpec,
		`{name: pageSize, in: query, schema: {type: integer}}`,
		`{name: pageSize, in: query, schema: {type: integer, minimum: 1, maximum: 100}}`,
		1,
	)
	server, err := BuildServer([]byte(specYAML), Options{BaseURL: "http://example.test", Org: "x"})
	require.NoError(t, err)
	ts := httptest.NewServer(NewHandler(server))
	defer ts.Close()

	resp := mcpPost(t, ts.URL, "", jsonrpcRequest(2, "tools/call", map[string]any{
		"name":      "nico_get_all_foo",
		"arguments": map[string]any{"pageSize": 101},
	}))
	defer resp.Body.Close()
	require.Equal(t, http.StatusOK, resp.StatusCode)

	body, err := io.ReadAll(resp.Body)
	require.NoError(t, err)
	var env struct {
		Error *struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	require.NoError(t, json.Unmarshal(body, &env))
	require.NotNil(t, env.Error)
	require.Contains(t, env.Error.Message, "invalid params")
	require.Contains(t, env.Error.Message, "pageSize")
}

func TestHandler_ToolsCall_RejectsUnknownArg(t *testing.T) {
	server, err := BuildServer([]byte(synthSpec), Options{BaseURL: "http://example.test", Org: "x"})
	require.NoError(t, err)
	ts := httptest.NewServer(NewHandler(server))
	defer ts.Close()

	resp := mcpPost(t, ts.URL, "", jsonrpcRequest(2, "tools/call", map[string]any{
		"name":      "nico_get_all_foo",
		"arguments": map[string]any{"page_size": 1},
	}))
	defer resp.Body.Close()
	require.Equal(t, http.StatusOK, resp.StatusCode)

	body, err := io.ReadAll(resp.Body)
	require.NoError(t, err)
	var env struct {
		Error *struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	require.NoError(t, json.Unmarshal(body, &env))
	require.NotNil(t, env.Error)
	require.Contains(t, env.Error.Message, "invalid params")
	require.Contains(t, env.Error.Message, "page_size")
}

func TestHandler_ToolsCall_BearerPassthrough(t *testing.T) {
	var (
		recordedAuth atomic.Value
		recordedPath atomic.Value
	)
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		recordedAuth.Store(r.Header.Get("Authorization"))
		recordedPath.Store(r.URL.Path)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"id":"foo-1","name":"Foo One"}`))
	}))
	defer upstream.Close()

	server, err := BuildServer([]byte(synthSpec), Options{
		BaseURL: upstream.URL,
		Org:     "tester",
		APIName: "nico",
	})
	require.NoError(t, err)
	ts := httptest.NewServer(NewHandler(server))
	defer ts.Close()

	resp := mcpPost(t, ts.URL, "Bearer caller-jwt-xyz", jsonrpcRequest(2, "tools/call", map[string]any{
		"name":      "nico_get_foo",
		"arguments": map[string]any{"fooId": "foo-1"},
	}))
	defer resp.Body.Close()

	require.Equal(t, http.StatusOK, resp.StatusCode)
	body, err := io.ReadAll(resp.Body)
	require.NoError(t, err)

	result := decodeToolCallResult(t, body)
	require.False(t, result.IsError, "tool call should succeed: %s", body)
	require.Equal(t, `{"id":"foo-1","name":"Foo One"}`, firstText(result))

	require.Equal(t, "Bearer caller-jwt-xyz", recordedAuth.Load())
	require.Equal(t, "/v2/org/tester/nico/foo/foo-1", recordedPath.Load())
}

func TestHandler_ToolsCall_TokenArgWins(t *testing.T) {
	var recordedAuth atomic.Value
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		recordedAuth.Store(r.Header.Get("Authorization"))
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`[]`))
	}))
	defer upstream.Close()

	server, err := BuildServer([]byte(synthSpec), Options{
		BaseURL: upstream.URL,
		Org:     "tester",
		APIName: "nico",
	})
	require.NoError(t, err)
	ts := httptest.NewServer(NewHandler(server))
	defer ts.Close()

	resp := mcpPost(t, ts.URL, "Bearer inbound-bearer", jsonrpcRequest(3, "tools/call", map[string]any{
		"name": "nico_get_all_foo",
		"arguments": map[string]any{
			"token": "explicit-tool-arg-token",
		},
	}))
	defer resp.Body.Close()

	require.Equal(t, http.StatusOK, resp.StatusCode)
	require.Equal(t, "Bearer explicit-tool-arg-token", recordedAuth.Load())
}

func TestHandler_ToolsCall_RejectsUnapprovedCredentialDestination(t *testing.T) {
	tests := []struct {
		name          string
		opts          Options
		authorization string
		wantError     string
	}{
		{
			name:          "configured_base_url_with_inbound_bearer",
			opts:          Options{BaseURL: "https://configured.example.com", Org: "tester"},
			authorization: "Bearer inbound-bearer",
			wantError:     "does not match the configured server base URL",
		},
		{
			name:      "configured_base_url_with_default_token",
			opts:      Options{BaseURL: "https://configured.example.com", Org: "tester", Token: "default-token"},
			wantError: "does not match the configured server base URL",
		},
		{
			name:          "dynamic_base_url_with_inbound_bearer",
			opts:          Options{Org: "tester"},
			authorization: "Bearer inbound-bearer",
			wantError:     "refusing to forward inherited credentials",
		},
		{
			name:      "dynamic_base_url_with_default_token",
			opts:      Options{Org: "tester", Token: "default-token"},
			wantError: "refusing to forward inherited credentials",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var requests atomic.Int64
			capture := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				requests.Add(1)
				w.Header().Set("Content-Type", "application/json")
				_, _ = w.Write([]byte(`[]`))
			}))
			defer capture.Close()

			server, err := BuildServer([]byte(synthSpec), tt.opts)
			require.NoError(t, err)
			ts := httptest.NewServer(NewHandler(server))
			defer ts.Close()

			resp := mcpPost(t, ts.URL, tt.authorization, jsonrpcRequest(4, "tools/call", map[string]any{
				"name": "nico_get_all_foo",
				"arguments": map[string]any{
					"base_url": capture.URL,
				},
			}))
			defer resp.Body.Close()
			require.Equal(t, http.StatusOK, resp.StatusCode)

			body, err := io.ReadAll(resp.Body)
			require.NoError(t, err)
			result := decodeToolCallResult(t, body)
			require.True(t, result.IsError, "tool call should be rejected: %s", body)
			require.Contains(t, firstText(result), tt.wantError)
			require.Zero(t, requests.Load(), "unapproved destination received an outbound request")
		})
	}
}

func TestHandler_ToolsCall_DynamicDestinationUsesExplicitToken(t *testing.T) {
	var recordedAuth atomic.Value
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		recordedAuth.Store(r.Header.Get("Authorization"))
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`[]`))
	}))
	defer upstream.Close()

	server, err := BuildServer([]byte(synthSpec), Options{Org: "tester", Token: "default-token"})
	require.NoError(t, err)
	ts := httptest.NewServer(NewHandler(server))
	defer ts.Close()

	resp := mcpPost(t, ts.URL, "Bearer inbound-bearer", jsonrpcRequest(5, "tools/call", map[string]any{
		"name": "nico_get_all_foo",
		"arguments": map[string]any{
			"base_url": upstream.URL,
			"token":    "explicit-tool-arg-token",
		},
	}))
	defer resp.Body.Close()
	require.Equal(t, http.StatusOK, resp.StatusCode)

	body, err := io.ReadAll(resp.Body)
	require.NoError(t, err)
	result := decodeToolCallResult(t, body)
	require.False(t, result.IsError, "tool call should succeed: %s", body)
	require.Equal(t, "Bearer explicit-tool-arg-token", recordedAuth.Load())
}

func TestHandler_ToolsCall_CrossOriginRedirectRejected(t *testing.T) {
	for _, tt := range []struct {
		name          string
		authorization string
	}{
		{name: "authenticated", authorization: "Bearer caller-jwt"},
		{name: "unauthenticated"},
	} {
		t.Run(tt.name, func(t *testing.T) {
			var (
				captureRequests atomic.Int64
				captureAuth     atomic.Value
				sourceAuth      atomic.Value
			)
			captureAuth.Store("")
			sourceAuth.Store("")
			capture := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				captureRequests.Add(1)
				captureAuth.Store(r.Header.Get("Authorization"))
				w.Header().Set("Content-Type", "application/json")
				_, _ = w.Write([]byte(`[]`))
			}))
			defer capture.Close()

			source := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				sourceAuth.Store(r.Header.Get("Authorization"))
				http.Redirect(w, r, capture.URL, http.StatusTemporaryRedirect)
			}))
			defer source.Close()

			server, err := BuildServer([]byte(synthSpec), Options{BaseURL: source.URL, Org: "tester"})
			require.NoError(t, err)
			ts := httptest.NewServer(NewHandler(server))
			defer ts.Close()

			resp := mcpPost(t, ts.URL, tt.authorization, jsonrpcRequest(6, "tools/call", map[string]any{
				"name":      "nico_get_all_foo",
				"arguments": map[string]any{},
			}))
			defer resp.Body.Close()
			require.Equal(t, http.StatusOK, resp.StatusCode)

			body, err := io.ReadAll(resp.Body)
			require.NoError(t, err)
			result := decodeToolCallResult(t, body)
			require.True(t, result.IsError, "tool call should be rejected: %s", body)
			require.Contains(t, firstText(result), "refusing cross-origin redirect")
			require.Equal(t, tt.authorization, sourceAuth.Load())
			require.Zero(t, captureRequests.Load(), "cross-origin redirect destination received a request")
			require.Empty(t, captureAuth.Load(), "cross-origin redirect destination received Authorization")
		})
	}
}

func TestHandler_ToolsCall_SameOriginRedirectAllowed(t *testing.T) {
	var (
		requests       atomic.Int64
		redirectedAuth atomic.Value
	)
	redirectedAuth.Store("")
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests.Add(1)
		if r.URL.Path != "/redirected" {
			http.Redirect(w, r, "/redirected", http.StatusTemporaryRedirect)
			return
		}
		redirectedAuth.Store(r.Header.Get("Authorization"))
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`[]`))
	}))
	defer upstream.Close()

	server, err := BuildServer([]byte(synthSpec), Options{BaseURL: upstream.URL, Org: "tester", Token: "default-token"})
	require.NoError(t, err)
	ts := httptest.NewServer(NewHandler(server))
	defer ts.Close()

	resp := mcpPost(t, ts.URL, "", jsonrpcRequest(7, "tools/call", map[string]any{
		"name":      "nico_get_all_foo",
		"arguments": map[string]any{},
	}))
	defer resp.Body.Close()
	require.Equal(t, http.StatusOK, resp.StatusCode)

	body, err := io.ReadAll(resp.Body)
	require.NoError(t, err)
	result := decodeToolCallResult(t, body)
	require.False(t, result.IsError, "tool call should succeed: %s", body)
	require.Equal(t, int64(2), requests.Load())
	require.Equal(t, "Bearer default-token", redirectedAuth.Load())
}

// TestHandler_ConcurrentCallersDoNotBleedTokens proves the
// statelessness invariant: two callers hitting the same handler with
// different bearers each get their own bearer forwarded to NICo REST.
// Run in parallel to also stress for shared-state races.
func TestHandler_ConcurrentCallersDoNotBleedTokens(t *testing.T) {
	t.Parallel()

	var (
		mu      sync.Mutex
		perPath = map[string]string{}
	)
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		perPath[r.URL.Path] = r.Header.Get("Authorization")
		mu.Unlock()
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{}`))
	}))
	defer upstream.Close()

	server, err := BuildServer([]byte(synthSpec), Options{
		BaseURL: upstream.URL,
		Org:     "tester",
		APIName: "nico",
	})
	require.NoError(t, err)
	ts := httptest.NewServer(NewHandler(server))
	defer ts.Close()

	const callers = 8
	wg := sync.WaitGroup{}
	for i := 0; i < callers; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			id := i + 100
			resp := mcpPost(t, ts.URL,
				"Bearer caller-"+itoa(i),
				jsonrpcRequest(id, "tools/call", map[string]any{
					"name": "nico_get_foo",
					"arguments": map[string]any{
						"fooId": "foo-" + itoa(i),
					},
				}))
			defer resp.Body.Close()
			require.Equal(t, http.StatusOK, resp.StatusCode)
		}(i)
	}
	wg.Wait()

	mu.Lock()
	defer mu.Unlock()
	require.Len(t, perPath, callers, "each caller should have hit a distinct path")
	for path, auth := range perPath {
		// path looks like /v2/org/tester/nico/foo/foo-<i>; auth must
		// match its caller's bearer.
		idx := strings.TrimPrefix(path, "/v2/org/tester/nico/foo/foo-")
		require.Equal(t, "Bearer caller-"+idx, auth, "bearer leaked between callers on %s", path)
	}
}

// --- helpers below ---

func mcpPost(t *testing.T, base string, authorization string, body []byte) *http.Response {
	t.Helper()
	req, err := http.NewRequest(http.MethodPost, base, bytes.NewReader(body))
	require.NoError(t, err)
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json, text/event-stream")
	if authorization != "" {
		req.Header.Set("Authorization", authorization)
	}
	resp, err := http.DefaultClient.Do(req)
	require.NoError(t, err)
	return resp
}

func jsonrpcRequest(id int, method string, params map[string]any) []byte {
	type wrapper struct {
		JSONRPC string         `json:"jsonrpc"`
		ID      int            `json:"id"`
		Method  string         `json:"method"`
		Params  map[string]any `json:"params"`
	}
	b, err := json.Marshal(wrapper{
		JSONRPC: "2.0",
		ID:      id,
		Method:  method,
		Params:  params,
	})
	if err != nil {
		panic(err)
	}
	return b
}

type rpcTool struct {
	Name        string `json:"name"`
	Description string `json:"description"`
}

func decodeToolList(t *testing.T, body []byte) []rpcTool {
	t.Helper()
	var env struct {
		Result struct {
			Tools []rpcTool `json:"tools"`
		} `json:"result"`
	}
	require.NoError(t, json.Unmarshal(body, &env))
	return env.Result.Tools
}

type rpcContent struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

type rpcToolCallResult struct {
	IsError bool         `json:"isError"`
	Content []rpcContent `json:"content"`
}

func decodeToolCallResult(t *testing.T, body []byte) rpcToolCallResult {
	t.Helper()
	var env struct {
		Result rpcToolCallResult `json:"result"`
	}
	require.NoError(t, json.Unmarshal(body, &env))
	return env.Result
}

func firstText(r rpcToolCallResult) string {
	for _, c := range r.Content {
		if c.Type == "text" {
			return c.Text
		}
	}
	return ""
}

func itoa(i int) string {
	if i == 0 {
		return "0"
	}
	var b strings.Builder
	if i < 0 {
		b.WriteByte('-')
		i = -i
	}
	digits := []byte{}
	for i > 0 {
		digits = append([]byte{byte('0' + i%10)}, digits...)
		i /= 10
	}
	b.Write(digits)
	return b.String()
}
