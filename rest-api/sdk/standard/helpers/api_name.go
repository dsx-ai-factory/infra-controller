// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package helpers

import (
	"net/http"
	"regexp"
	"strings"
)

// DefaultAPIName is the default API path segment used in org-scoped routes.
const DefaultAPIName = "nico"

var orgScopedAPIPathPattern = regexp.MustCompile(`(/v[0-9]+/org/[^/]+/)([^/]+)`)

// APINameRewriteTransport is an http.RoundTripper that rewrites the API path
// segment in org-scoped URLs before forwarding the request.
type APINameRewriteTransport struct {
	apiName string
	next    http.RoundTripper
}

// NewAPINameRewriteTransport returns a transport that rewrites the API path
// segment after /org/{org}/ to the given apiName.
func NewAPINameRewriteTransport(apiName string, next http.RoundTripper) *APINameRewriteTransport {
	return &APINameRewriteTransport{apiName: NormalizeAPIName(apiName), next: next}
}

// SetAPIName updates the API path segment used for rewriting.
func (t *APINameRewriteTransport) SetAPIName(apiName string) {
	t.apiName = NormalizeAPIName(apiName)
}

// APIName returns the configured API path segment.
func (t *APINameRewriteTransport) APIName() string {
	return t.apiName
}

// RoundTrip rewrites the request URL path to replace the API segment and
// delegates to the wrapped transport.
func (t *APINameRewriteTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	rewrittenReq := req
	if req != nil {
		rewrittenPath := RewriteAPINamePath(req.URL.Path, t.apiName)
		rewrittenRawPath := req.URL.RawPath
		if rewrittenRawPath != "" {
			rewrittenRawPath = RewriteAPINamePath(req.URL.RawPath, t.apiName)
		}
		if rewrittenPath != req.URL.Path || rewrittenRawPath != req.URL.RawPath {
			reqCopy := req.Clone(req.Context())
			urlCopy := *req.URL
			reqCopy.URL = &urlCopy
			reqCopy.URL.Path = rewrittenPath
			reqCopy.URL.RawPath = rewrittenRawPath
			rewrittenReq = reqCopy
		}
	}

	transport := t.next
	if transport == nil {
		transport = http.DefaultTransport
	}
	return transport.RoundTrip(rewrittenReq)
}

// CurrentAPINameRewriteTransport extracts the APINameRewriteTransport from the
// given http.Client, if one is installed.
func CurrentAPINameRewriteTransport(client *http.Client) (*APINameRewriteTransport, bool) {
	if client == nil {
		return nil, false
	}
	rewriter, ok := client.Transport.(*APINameRewriteTransport)
	return rewriter, ok
}

// NormalizeAPIName trims whitespace and slashes from apiName and returns
// DefaultAPIName when the result is empty or contains path separators.
func NormalizeAPIName(apiName string) string {
	apiName = strings.TrimSpace(apiName)
	apiName = strings.Trim(apiName, "/")
	if apiName == "" || strings.Contains(apiName, "/") {
		return DefaultAPIName
	}
	return apiName
}

// RewriteAPINamePath replaces the API path segment in org-scoped URL paths
// (e.g. /v2/org/{org}/{api}/...) with the given apiName.
func RewriteAPINamePath(path, apiName string) string {
	apiName = NormalizeAPIName(apiName)
	if path == "" || apiName == DefaultAPIName {
		return path
	}
	return orgScopedAPIPathPattern.ReplaceAllString(path, "${1}"+apiName)
}
