// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"encoding/json"
	"net/url"
	"strconv"
	"strings"
	"unicode"

	"github.com/modelcontextprotocol/go-sdk/mcp"
)

// stringArg returns the trimmed string value for key in a tool-call
// argument map, or "" when the key is absent or not a string. Reading a
// nil map or a missing key yields the zero value, and the comma-ok type
// assertion yields "" for any non-string value.
func stringArg(in map[string]any, key string) string {
	s, _ := in[key].(string)
	return strings.TrimSpace(s)
}

// normalizeBaseURL trims trailing slashes so joining the base with an
// absolute "/v2/..." path never yields a double slash.
func normalizeBaseURL(v string) string {
	return strings.TrimRight(v, "/")
}

func sameBaseURL(a, b string) bool {
	left, err := url.Parse(a)
	if err != nil {
		return false
	}
	right, err := url.Parse(b)
	if err != nil {
		return false
	}
	if !sameOrigin(left, right) || (left.User == nil) != (right.User == nil) {
		return false
	}
	if left.User != nil && left.User.String() != right.User.String() {
		return false
	}
	left.Scheme, left.Host, left.User = "", "", nil
	right.Scheme, right.Host, right.User = "", "", nil
	return *left == *right
}

// normalizeToken strips a leading "Bearer " scheme (case-insensitive) so
// callers can pass either a raw token or a full Authorization value.
func normalizeToken(v string) string {
	const prefix = "Bearer "
	if len(v) > len(prefix) && strings.EqualFold(v[:len(prefix)], prefix) {
		return strings.TrimSpace(v[len(prefix):])
	}
	return v
}

func toSnakeCase(s string) string {
	var b strings.Builder
	var prev rune
	for i, r := range s {
		switch {
		case unicode.IsUpper(r):
			if i > 0 && (unicode.IsLower(prev) || unicode.IsDigit(prev)) {
				b.WriteByte('_')
			}
			b.WriteRune(unicode.ToLower(r))
		case unicode.IsLetter(r) || unicode.IsDigit(r):
			b.WriteRune(r)
		default:
			b.WriteByte('_')
		}
		prev = r
	}
	out := b.String()
	for strings.Contains(out, "__") {
		out = strings.ReplaceAll(out, "__", "_")
	}
	return strings.Trim(out, "_")
}

func coerceToString(v any) (string, bool) {
	switch t := v.(type) {
	case nil:
		return "", true
	case string:
		return t, true
	case bool:
		return strconv.FormatBool(t), true
	case float64:
		// JSON numbers decode to float64; format integers without the
		// decimal point so they round-trip through query strings.
		if t == float64(int64(t)) {
			return strconv.FormatInt(int64(t), 10), true
		}
		return strconv.FormatFloat(t, 'g', -1, 64), true
	case int:
		return strconv.Itoa(t), true
	case int64:
		return strconv.FormatInt(t, 10), true
	case json.Number:
		return t.String(), true
	default:
		return "", false
	}
}

// bearerFromExtra extracts the bearer from a *mcp.CallToolRequest's
// inbound HTTP headers. The streamable-HTTP handler stamps every JSON-RPC
// request with req.Extra.Header from the HTTP request. Returns the bare
// token without the "Bearer " prefix; returns "" for any value the SDK
// did not stash or that does not look like a bearer.
func bearerFromExtra(req *mcp.CallToolRequest) string {
	if req == nil || req.Extra == nil || req.Extra.Header == nil {
		return ""
	}
	auth := req.Extra.Header.Get("Authorization")
	if auth == "" {
		return ""
	}
	const prefix = "Bearer "
	if len(auth) <= len(prefix) || !strings.EqualFold(auth[:len(prefix)], prefix) {
		return ""
	}
	return strings.TrimSpace(auth[len(prefix):])
}
