// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"cmp"
	"fmt"
	"strings"

	"github.com/modelcontextprotocol/go-sdk/mcp"
	"github.com/sirupsen/logrus"
)

// Options carry the server-side settings that tool invocations resolve against
// through FromCallConfig. BaseURL also bounds the allowed destination.
type Options struct {
	// BaseURL is the trusted NICo REST base URL (e.g. https://nico.example.com).
	// When set, a per-call base_url must resolve to the same value.
	BaseURL string
	// Org is the default organisation used in /v2/org/<org>/... paths.
	Org string
	// APIName is the API path segment between org and resource (default
	// "nico", overridable via api.name in config).
	APIName string
	// Token is the static bearer used with BaseURL when no inbound bearer or
	// tool arg token is provided. It is not sent to a caller-selected base URL.
	Token string
	// Debug enables logrus debug-level HTTP request/response logging
	// through to the appcli.Client.
	Debug bool
	// Log is the logrus entry used for client request/response logging.
	// If nil, a default entry wrapping the standard logger is used.
	Log *logrus.Entry
}

// withDefaults returns a copy of opts with empty optional fields filled
// in with package defaults. APIName falls back to "nico" and Log to
// logrus.StandardLogger() so callers can leave them unset.
func (o Options) withDefaults() Options {
	if o.APIName == "" {
		o.APIName = "nico"
	}
	if o.Log == nil {
		o.Log = logrus.NewEntry(logrus.StandardLogger())
	}
	return o
}

// commonConfigDescriptions documents the four per-call config fields that are
// merged into every tool's input schema. Kept as a slice (not
// a map) so the schema render order is stable.
var commonConfigDescriptions = []struct {
	Name string
	Desc string
}{
	{"org", "Org used in /v2/org/<org>/... paths for this call. Overrides the server startup flag/env default when set."},
	{"base_url", "NICo REST base URL for this call. When the server has a configured base URL, this value must match it. Otherwise, only a token supplied in the same tool call may be sent to this destination."},
	{"api_name", "Override the API path segment used in /v2/org/<org>/<name>/... (api.name; default \"nico\")."},
	{"token", "Bearer token for this call. Overrides the inbound Authorization header. Inbound or default credentials are forwarded only to the server's configured base URL."},
}

// resolvedConfig is the result of resolving Options with the per-call values
// for one tool invocation. It is consumed by registerGET to construct a fresh
// appcli.Client.
type resolvedConfig struct {
	BaseURL string
	Org     string
	APIName string
	Token   string
}

// FromCallConfig populates cfg by resolving the precedence chain
// documented in the design plan:
//
//  1. Tool-call argument (org, base_url, api_name, token)
//  2. Inbound Authorization header (token only, for a configured BaseURL)
//  3. Server startup flag / Options (BaseURL, Org, APIName, Token)
//
// A configured BaseURL binds all calls to that destination. Without one,
// inherited inbound or default credentials are not accepted for a per-call
// destination; if authentication is needed, callers must provide the token in
// the same tool call. It returns an error when this policy is violated or a
// required field (org, base_url) ends up empty, before the handler constructs
// an outbound request.
func (cfg *resolvedConfig) FromCallConfig(in map[string]any, req *mcp.CallToolRequest, opts Options) error {
	callBaseURL := normalizeBaseURL(stringArg(in, "base_url"))
	configuredBaseURL := normalizeBaseURL(opts.BaseURL)
	if configuredBaseURL != "" && callBaseURL != "" && !sameBaseURL(callBaseURL, configuredBaseURL) {
		return fmt.Errorf("per-call base_url does not match the configured server base URL")
	}

	callToken := stringArg(in, "token")
	inboundToken := bearerFromExtra(req)
	if configuredBaseURL == "" && callBaseURL != "" && callToken == "" &&
		(inboundToken != "" || opts.Token != "") {
		return fmt.Errorf("refusing to forward inherited credentials to a per-call base_url; pass token in the same tool call or configure the server base URL")
	}

	cfg.BaseURL = cmp.Or(callBaseURL, configuredBaseURL)
	cfg.Org = cmp.Or(stringArg(in, "org"), opts.Org)
	cfg.APIName = cmp.Or(stringArg(in, "api_name"), opts.APIName)
	cfg.Token = normalizeToken(cmp.Or(
		callToken,
		inboundToken,
		opts.Token,
	))
	return cfg.requireNonEmpty()
}

// requireNonEmpty returns a descriptive error when org or BaseURL are blank.
// Token may be empty; appcli.Client then sends no Authorization header.
func (c resolvedConfig) requireNonEmpty() error {
	missing := []string{}
	if c.Org == "" {
		missing = append(missing, "org")
	}
	if c.BaseURL == "" {
		missing = append(missing, "base_url")
	}
	if len(missing) == 0 {
		return nil
	}
	return fmt.Errorf("missing required config value(s): %s; pass via tool-call arguments, server flags, or NICO_* environment variables",
		strings.Join(missing, ", "))
}
