// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"net/http"
	"testing"

	"github.com/modelcontextprotocol/go-sdk/mcp"
	"github.com/stretchr/testify/require"
)

func TestStringArg(t *testing.T) {
	in := map[string]any{
		"a": "hello",
		"b": "  hello  ",
		"c": 42,
		"d": nil,
		"e": "",
	}
	require.Equal(t, "hello", stringArg(in, "a"))
	require.Equal(t, "hello", stringArg(in, "b"))
	require.Equal(t, "", stringArg(in, "c"))
	require.Equal(t, "", stringArg(in, "d"))
	require.Equal(t, "", stringArg(in, "e"))
	require.Equal(t, "", stringArg(in, "missing"))
	require.Equal(t, "", stringArg(nil, "any"))
}

func TestNormalizeBaseURL(t *testing.T) {
	require.Equal(t, "https://api.example.com", normalizeBaseURL("https://api.example.com/"))
	require.Equal(t, "https://api.example.com", normalizeBaseURL("https://api.example.com///"))
	require.Equal(t, "https://api.example.com/v2", normalizeBaseURL("https://api.example.com/v2/"))
	require.Equal(t, "", normalizeBaseURL(""))
}

func TestNormalizeToken(t *testing.T) {
	require.Equal(t, "abc.def", normalizeToken("Bearer abc.def"))
	require.Equal(t, "abc.def", normalizeToken("bearer abc.def"))
	require.Equal(t, "abc.def", normalizeToken("Bearer   abc.def   "))
	require.Equal(t, "abc.def", normalizeToken("abc.def"))
}

func TestBearerFromExtra(t *testing.T) {
	cases := []struct {
		name string
		hdr  http.Header
		want string
	}{
		{"nil_req", nil, ""},
		{"empty_header", http.Header{}, ""},
		{"bearer", http.Header{"Authorization": []string{"Bearer abc.def"}}, "abc.def"},
		{"bearer_lowercase_scheme", http.Header{"Authorization": []string{"bearer abc.def"}}, "abc.def"},
		{"bearer_with_padding", http.Header{"Authorization": []string{"Bearer   spaced   "}}, "spaced"},
		{"non_bearer_basic", http.Header{"Authorization": []string{"Basic dXNlcjpwYXNz"}}, ""},
		{"empty_value", http.Header{"Authorization": []string{""}}, ""},
		{"bearer_alone", http.Header{"Authorization": []string{"Bearer "}}, ""},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			var req *mcp.CallToolRequest
			if c.hdr != nil {
				req = &mcp.CallToolRequest{Extra: &mcp.RequestExtra{Header: c.hdr}}
			}
			require.Equal(t, c.want, bearerFromExtra(req))
		})
	}
}
