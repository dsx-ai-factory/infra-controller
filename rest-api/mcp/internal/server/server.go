// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package server serves the NICo REST read surface as MCP tools over
// streamable-HTTP. Tools are projected 1:1 from the embedded OpenAPI
// spec's GET operations. The server is stateless and never emits SSE:
// every tool/call returns a single application/json body.
package server

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"net/http"
	"net/netip"
	"net/url"
	"os"
	"os/signal"
	"slices"
	"strings"
	"syscall"
	"time"

	"github.com/getkin/kin-openapi/openapi3"
	"github.com/modelcontextprotocol/go-sdk/mcp"
	"github.com/sirupsen/logrus"
	urfave "github.com/urfave/cli/v2"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
)

// BuildServer constructs an *mcp.Server with one tool registered for
// every GET operation in the supplied OpenAPI spec. Tool names follow
// the SDD: nico_<snake_case(operationId)>. Each tool handler builds a
// fresh appcli.Client per call from resolvedConfig.FromCallConfig. Inbound
// bearer tokens are forwarded unchanged only to the configured NICo REST
// base URL.
//
// BuildServer does not start a listener; callers wrap the result with
// NewHandler to get an *http.Handler ready for ListenAndServe.
func BuildServer(specData []byte, opts Options) (*mcp.Server, error) {
	loader := openapi3.NewLoader()
	doc, err := loader.LoadFromData(specData)
	if err != nil {
		return nil, fmt.Errorf("parsing spec: %w", err)
	}
	opts = opts.withDefaults()

	version := ""
	if doc.Info != nil {
		version = doc.Info.Version
	}
	server := mcp.NewServer(&mcp.Implementation{
		Name:    "nico-mcp",
		Title:   "NVIDIA Infrastructure Controller (NICo) MCP",
		Version: version,
	}, nil)

	paths := doc.Paths.Map()
	for _, path := range sortedPaths(paths) {
		item := paths[path]
		if item.Get == nil || item.Get.OperationID == "" {
			continue
		}
		registerGET(server, path, item, opts)
	}
	return server, nil
}

// NewHandler wraps an *mcp.Server in a streamable-HTTP handler
// configured for stateless, request/response-only operation:
//
//   - Stateless: true   -- the SDK does not validate Mcp-Session-Id
//     and rejects server->client requests. initialize is a no-op.
//   - JSONResponse: true -- every tool/call response uses
//     Content-Type: application/json; the SDK never opens an SSE
//     stream, so clients and intermediaries that do not speak
//     text/event-stream still receive a single JSON body.
//
// DNS-rebinding (localhost) protection and cross-origin protection are
// deliberately left at the SDK's secure defaults (go-sdk v1.4.1+):
// browser cross-origin requests and localhost DNS-rebinding attempts are
// rejected, while non-browser MCP clients -- which send no Origin
// or Sec-Fetch-Site header -- pass through unaffected. Do not set
// DisableLocalhostProtection or a permissive CrossOriginProtection here
// without understanding the security trade-off.
func NewHandler(server *mcp.Server) http.Handler {
	return mcp.NewStreamableHTTPHandler(
		func(*http.Request) *mcp.Server { return server },
		&mcp.StreamableHTTPOptions{
			Stateless:    true,
			JSONResponse: true,
		},
	)
}

// ServeFlags returns the full flag set for the standalone nico-mcp server
// binary. The server-specific flags (listen, path, shutdown-timeout) sit
// alongside the connection flags (base-url, org, api-name, token, debug).
// The dynamically-generated CLI exposes the latter as
// root-level flags, but nico-mcp is a single-purpose binary with no parent
// command to inherit from, so it declares all of them itself. Each flag also
// reads its NICO_* environment variable.
func ServeFlags() []urfave.Flag {
	return []urfave.Flag{
		&urfave.StringFlag{
			Name:    "listen",
			Usage:   "address:port to listen on",
			EnvVars: []string{"NICO_MCP_LISTEN"},
			Value:   ":8080",
		},
		&urfave.StringFlag{
			Name:    "path",
			Usage:   "HTTP path the MCP handler is mounted at",
			EnvVars: []string{"NICO_MCP_PATH"},
			Value:   "/mcp",
		},
		&urfave.DurationFlag{
			Name:    "shutdown-timeout",
			Usage:   "graceful shutdown timeout when SIGINT/SIGTERM arrives",
			EnvVars: []string{"NICO_MCP_SHUTDOWN_TIMEOUT"},
			Value:   10 * time.Second,
		},
		&urfave.StringFlag{
			Name:    "base-url",
			Usage:   "trusted NICo REST base URL (a per-call base_url must match when this is set)",
			EnvVars: []string{"NICO_BASE_URL"},
		},
		&urfave.StringFlag{
			Name:    "org",
			Usage:   "default org used in /v2/org/<org>/... paths (a per-call org argument overrides this)",
			EnvVars: []string{"NICO_ORG"},
		},
		&urfave.StringFlag{
			Name:    "api-name",
			Usage:   "API path segment used in /v2/org/<org>/<name>/... routes",
			EnvVars: []string{"NICO_API_NAME"},
			Value:   "nico",
		},
		&urfave.StringFlag{
			Name:    "token",
			Usage:   "default bearer token for the configured base URL (a per-call token or inbound Authorization header overrides this)",
			EnvVars: []string{"NICO_TOKEN"},
		},
		&urfave.BoolFlag{
			Name:  "debug",
			Usage: "enable debug logging (full HTTP request/response)",
		},
	}
}

// Run wires the urfave context into Options, builds the MCP server, and runs
// an http.Server until SIGINT/SIGTERM. It is the action backing the standalone
// nico-mcp binary, exported so the binary's main stays a thin wrapper and so
// tests can drive it directly.
func Run(c *urfave.Context, specData []byte) error {
	opts := buildServeOptions(c)

	server, err := BuildServer(specData, opts)
	if err != nil {
		return fmt.Errorf("building MCP server: %w", err)
	}

	listen := c.String("listen")
	path := c.String("path")
	if path == "" || path[0] != '/' {
		return fmt.Errorf("invalid --path %q: must be non-empty and start with '/'", path)
	}
	shutdownTimeout := c.Duration("shutdown-timeout")

	mux := http.NewServeMux()
	if err := registerHandler(mux, path, NewHandler(server)); err != nil {
		return err
	}

	httpServer := &http.Server{
		Addr:              listen,
		Handler:           mux,
		ReadHeaderTimeout: 10 * time.Second,
		ReadTimeout:       30 * time.Second,
		WriteTimeout:      60 * time.Second,
		IdleTimeout:       120 * time.Second,
	}

	opts.Log.Infof("nico-mcp: listening on %s, MCP at %s (stateless, JSONResponse)", listen, path)

	errCh := make(chan error, 1)
	go func() {
		errCh <- httpServer.ListenAndServe()
	}()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
	defer signal.Stop(sigCh)

	select {
	case err := <-errCh:
		if err != nil && !errors.Is(err, http.ErrServerClosed) {
			return fmt.Errorf("http server: %w", err)
		}
		return nil
	case sig := <-sigCh:
		opts.Log.Infof("nico-mcp: received %s, shutting down", sig)
		ctx, cancel := context.WithTimeout(context.Background(), shutdownTimeout)
		defer cancel()
		if err := httpServer.Shutdown(ctx); err != nil {
			return fmt.Errorf("graceful shutdown: %w", err)
		}
		return nil
	}
}

// sortedPaths returns the path keys in deterministic order so the
// resulting tool list is stable across server restarts.
func sortedPaths(paths map[string]*openapi3.PathItem) []string {
	return slices.Sorted(maps.Keys(paths))
}

func registerGET(server *mcp.Server, path string, item *openapi3.PathItem, opts Options) {
	op := item.Get
	h := &NicoOpenApiHandler{}
	allParams := mergeParameters(item, op)

	tool := &mcp.Tool{
		Name:        toolName(op.OperationID),
		Description: toolDescription(op),
		InputSchema: h.buildInput(item, op),
		Annotations: &mcp.ToolAnnotations{
			ReadOnlyHint: true,
			Title:        op.Summary,
		},
	}

	mcp.AddTool(server, tool, func(ctx context.Context, req *mcp.CallToolRequest, in map[string]any) (*mcp.CallToolResult, any, error) {
		var cfg resolvedConfig
		if err := cfg.FromCallConfig(in, req, opts); err != nil {
			return errorResult(err), nil, nil
		}
		client := appcli.NewClient(cfg.BaseURL, cfg.Org, cfg.Token, opts.Log, opts.Debug)
		client.HTTPClient.CheckRedirect = sameOriginRedirectPolicy
		client.APIName = cfg.APIName

		pathParams, queryParams, err := splitArgs(in, allParams)
		if err != nil {
			return errorResult(err), nil, nil
		}
		body, respHeader, err := client.Do(http.MethodGet, path, pathParams, queryParams, nil)
		if err != nil {
			return errorResult(err), nil, nil
		}
		return jsonResult(body, respHeader), nil, nil
	})
}

// sameOriginRedirectPolicy preserves net/http's redirect limit and the original
// Authorization header while refusing requests to a different origin.
func sameOriginRedirectPolicy(req *http.Request, via []*http.Request) error {
	if len(via) >= 10 {
		return errors.New("stopped after 10 redirects")
	}
	if len(via) == 0 {
		return nil
	}
	if !sameOrigin(via[0].URL, req.URL) {
		return errors.New("refusing cross-origin redirect")
	}
	// net/http can strip Authorization before this callback when the host's
	// spelling changes, including IPv6 notation or DNS name case. Restore it
	// only after checking the origin.
	authorization := via[0].Header.Get("Authorization")
	if authorization != "" {
		req.Header.Set("Authorization", authorization)
	}
	return nil
}

func sameOrigin(a, b *url.URL) bool {
	if !strings.EqualFold(a.Scheme, b.Scheme) || effectivePort(a) != effectivePort(b) {
		return false
	}
	aIP, aErr := netip.ParseAddr(a.Hostname())
	bIP, bErr := netip.ParseAddr(b.Hostname())
	if aErr == nil && bErr == nil {
		return aIP == bIP
	}
	return strings.EqualFold(a.Hostname(), b.Hostname())
}

func effectivePort(u *url.URL) string {
	if port := u.Port(); port != "" {
		return port
	}
	switch strings.ToLower(u.Scheme) {
	case "http":
		return "80"
	case "https":
		return "443"
	default:
		return ""
	}
}

// toolName converts an operationId to the SDD's canonical MCP tool name
// nico_<snake_case(operationId)>. It handles kebab-case (get-all-site ->
// nico_get_all_site) and camelCase (getFooStatus -> nico_get_foo_status)
// equally: any non-alphanumeric run becomes a single underscore, an
// underscore is inserted at lower/digit -> upper transitions, and the
// result is lowercased and trimmed of leading/trailing underscores.
func toolName(operationID string) string {
	return "nico_" + toSnakeCase(operationID)
}

func toolDescription(op *openapi3.Operation) string {
	parts := make([]string, 0, 2)
	if s := strings.TrimSpace(op.Summary); s != "" {
		parts = append(parts, s)
	}
	if s := strings.TrimSpace(op.Description); s != "" {
		parts = append(parts, s)
	}
	if len(parts) == 0 {
		return op.OperationID
	}
	return strings.Join(parts, "\n\n")
}

// splitArgs maps the tool input map onto path and query parameters
// using the OpenAPI parameter definitions. Common config keys (org,
// base_url, api_name, token) and any unrecognised keys are dropped:
// they are consumed by resolvedConfig.FromCallConfig, not the URL
// builder. The "org" path parameter is intentionally skipped here because
// appcli.Client.Do substitutes {org} from Client.Org, which
// FromCallConfig sets from the per-call override or config layer.
//
// TODO: full OpenAPI style/explode serialization for array and object
// parameters is intentionally deferred; unsupported shapes fail fast.
func splitArgs(in map[string]any, params []*openapi3.Parameter) (pathParams, queryParams map[string]string, err error) {
	pathParams = map[string]string{}
	queryParams = map[string]string{}
	for _, p := range params {
		if p.Name == "org" {
			continue
		}
		raw, ok := in[p.Name]
		if !ok {
			continue
		}
		s, ok := coerceToString(raw)
		if !ok {
			return nil, nil, fmt.Errorf("unsupported argument type for %q: %T", p.Name, raw)
		}
		if s == "" {
			continue
		}
		switch p.In {
		case "path":
			pathParams[p.Name] = s
		case "query":
			queryParams[p.Name] = s
		}
	}
	return pathParams, queryParams, nil
}

func errorResult(err error) *mcp.CallToolResult {
	return &mcp.CallToolResult{
		IsError: true,
		Content: []mcp.Content{&mcp.TextContent{Text: err.Error()}},
	}
}

// jsonResult wraps a successful REST response body as a single JSON text
// content block. When the upstream response carries pagination metadata
// (the X-Pagination header NICo REST sets on list endpoints), it is
// surfaced under the result's _meta.pagination so MCP clients can page
// without the metadata polluting the tool's primary JSON payload.
func jsonResult(body []byte, header http.Header) *mcp.CallToolResult {
	res := &mcp.CallToolResult{
		Content: []mcp.Content{&mcp.TextContent{Text: string(body)}},
	}
	if meta := paginationMeta(header); meta != nil {
		res.Meta = meta
	}
	return res
}

// paginationMeta extracts NICo REST's X-Pagination response header into an
// MCP _meta map. The header value is JSON (e.g.
// {"pageNumber":1,"pageSize":50,"total":1234,"orderBy":null}); it is
// parsed so clients get structured fields, falling back to the raw string
// if it is not valid JSON. Returns nil when the header is absent.
func paginationMeta(header http.Header) mcp.Meta {
	raw := header.Get("X-Pagination")
	if raw == "" {
		return nil
	}
	var parsed any
	if err := json.Unmarshal([]byte(raw), &parsed); err != nil {
		parsed = raw
	}
	return mcp.Meta{"pagination": parsed}
}

func registerHandler(mux *http.ServeMux, path string, handler http.Handler) (err error) {
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("invalid --path %q: %v", path, r)
		}
	}()
	mux.Handle(path, handler)
	return nil
}

// buildServeOptions resolves the MCP server's start-up defaults from the
// process flags (each of which also reads its NICO_* environment
// variable). Unlike the dynamically-generated CLI commands, nico-mcp does
// NOT read ~/.nico/config.yaml: the server is stateless and entirely
// parameter-driven, so connection details are resolved per tool call via
// resolvedConfig.FromCallConfig, with these flag values as fallbacks. A
// configured base URL binds requests and inherited credentials to that
// destination. This lets nico-mcp start cleanly with no config file present.
func buildServeOptions(c *urfave.Context) Options {
	log := logrus.NewEntry(logrus.StandardLogger())
	if c.Bool("debug") {
		log.Logger.SetLevel(logrus.DebugLevel)
	}

	return Options{
		BaseURL: c.String("base-url"),
		Org:     c.String("org"),
		APIName: c.String("api-name"),
		Token:   c.String("token"),
		Debug:   c.Bool("debug"),
		Log:     log,
	}
}
