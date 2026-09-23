// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"net/http"
	"testing"

	"github.com/modelcontextprotocol/go-sdk/mcp"
	"github.com/stretchr/testify/require"
)

func TestFromCallConfig_PrecedenceChain(t *testing.T) {
	type expect struct {
		baseURL, org, apiName, token string
		errContains                  string
	}
	cases := []struct {
		name     string
		in       map[string]any
		req      *mcp.CallToolRequest
		opts     Options
		expected expect
	}{
		{
			name: "tool_args_resolve_every_field_without_configured_base_url",
			in: map[string]any{
				"base_url": "https://from-arg.example.com/",
				"org":      "arg-org",
				"api_name": "arg-name",
				"token":    "Bearer arg-token",
			},
			req:  requestWithBearer("inbound-bearer"),
			opts: Options{Org: "opts-org", APIName: "opts-name", Token: "opts-token"},
			expected: expect{
				baseURL: "https://from-arg.example.com",
				org:     "arg-org",
				apiName: "arg-name",
				token:   "arg-token",
			},
		},
		{
			name: "configured_base_url_rejects_different_call_destination",
			in: map[string]any{
				"base_url": "https://from-arg.example.com",
				"org":      "arg-org",
				"token":    "arg-token",
			},
			req:  requestWithBearer("inbound-bearer"),
			opts: Options{BaseURL: "https://opts.example.com", Org: "opts-org", Token: "opts-token"},
			expected: expect{
				errContains: "does not match the configured server base URL",
			},
		},
		{
			name: "configured_base_url_accepts_matching_normalized_call_destination",
			in: map[string]any{
				"base_url": "https://opts.example.com///",
			},
			req:  requestWithBearer("from-header"),
			opts: Options{BaseURL: "https://opts.example.com/", Org: "opts-org", APIName: "nico", Token: "opts-token"},
			expected: expect{
				baseURL: "https://opts.example.com",
				org:     "opts-org",
				apiName: "nico",
				token:   "from-header",
			},
		},
		{
			name: "configured_base_url_accepts_matching_scheme_and_host_case",
			in: map[string]any{
				"base_url": "https://OPTS.example.com/v2/",
			},
			req:  requestWithBearer("from-header"),
			opts: Options{BaseURL: "HTTPS://opts.example.com/v2", Org: "opts-org", APIName: "nico"},
			expected: expect{
				baseURL: "https://OPTS.example.com/v2",
				org:     "opts-org",
				apiName: "nico",
				token:   "from-header",
			},
		},
		{
			name: "configured_base_url_accepts_matching_explicit_default_port",
			in: map[string]any{
				"base_url": "https://opts.example.com/v2",
			},
			req:  requestWithBearer("from-header"),
			opts: Options{BaseURL: "https://opts.example.com:443/v2", Org: "opts-org", APIName: "nico"},
			expected: expect{
				baseURL: "https://opts.example.com/v2",
				org:     "opts-org",
				apiName: "nico",
				token:   "from-header",
			},
		},
		{
			name: "configured_base_url_accepts_equivalent_ipv6_literal",
			in: map[string]any{
				"base_url": "http://[::1]:19080/v2",
			},
			req:  requestWithBearer("from-header"),
			opts: Options{BaseURL: "http://[0:0:0:0:0:0:0:1]:19080/v2", Org: "opts-org", APIName: "nico"},
			expected: expect{
				baseURL: "http://[::1]:19080/v2",
				org:     "opts-org",
				apiName: "nico",
				token:   "from-header",
			},
		},
		{
			name: "configured_base_url_rejects_different_path_case",
			in: map[string]any{
				"base_url": "https://opts.example.com/V2",
			},
			opts: Options{BaseURL: "https://opts.example.com/v2", Org: "opts-org", APIName: "nico"},
			expected: expect{
				errContains: "does not match the configured server base URL",
			},
		},
		{
			name: "dynamic_destination_rejects_inbound_bearer_without_call_token",
			in: map[string]any{
				"base_url": "https://from-arg.example.com",
			},
			req:  requestWithBearer("from-header"),
			opts: Options{Org: "opts-org", APIName: "nico"},
			expected: expect{
				errContains: "refusing to forward inherited credentials",
			},
		},
		{
			name: "dynamic_destination_rejects_default_token_without_call_token",
			in: map[string]any{
				"base_url": "https://from-arg.example.com",
			},
			opts: Options{Org: "opts-org", APIName: "nico", Token: "opts-token"},
			expected: expect{
				errContains: "refusing to forward inherited credentials",
			},
		},
		{
			name: "dynamic_destination_without_credentials_is_allowed",
			in: map[string]any{
				"base_url": "https://from-arg.example.com/",
			},
			opts: Options{Org: "opts-org", APIName: "nico"},
			expected: expect{
				baseURL: "https://from-arg.example.com",
				org:     "opts-org",
				apiName: "nico",
			},
		},
		{
			name: "inbound_bearer_wins_when_no_token_arg",
			in:   map[string]any{},
			req:  requestWithBearer("from-header"),
			opts: Options{BaseURL: "https://opts.example.com", Org: "opts-org", APIName: "nico", Token: "opts-token"},
			expected: expect{
				baseURL: "https://opts.example.com",
				org:     "opts-org",
				apiName: "nico",
				token:   "from-header",
			},
		},
		{
			name: "opts_token_used_when_no_arg_and_no_inbound",
			in:   map[string]any{},
			req:  nil,
			opts: Options{BaseURL: "https://opts.example.com", Org: "opts-org", APIName: "nico", Token: "opts-token"},
			expected: expect{
				baseURL: "https://opts.example.com",
				org:     "opts-org",
				apiName: "nico",
				token:   "opts-token",
			},
		},
		{
			name: "missing_org_errors",
			in:   map[string]any{},
			req:  nil,
			opts: Options{BaseURL: "https://opts.example.com"},
			expected: expect{
				errContains: "missing required config value(s): org",
			},
		},
		{
			name: "missing_base_url_errors",
			in:   map[string]any{},
			req:  nil,
			opts: Options{Org: "opts-org"},
			expected: expect{
				errContains: "missing required config value(s): base_url",
			},
		},
		{
			name: "missing_both_errors",
			in:   map[string]any{},
			req:  nil,
			opts: Options{},
			expected: expect{
				errContains: "missing required config value(s): org, base_url",
			},
		},
		{
			name: "api_name_defaults_when_unset",
			in:   map[string]any{},
			req:  nil,
			opts: Options{BaseURL: "https://opts.example.com", Org: "opts-org", Token: "t"}.withDefaults(),
			expected: expect{
				baseURL: "https://opts.example.com",
				org:     "opts-org",
				apiName: "nico",
				token:   "t",
			},
		},
		{
			name: "empty_string_arg_falls_through_to_opts",
			in: map[string]any{
				"org":   "",
				"token": "   ",
			},
			req: requestWithBearer("inbound"),
			opts: Options{
				BaseURL: "https://opts.example.com",
				Org:     "opts-org",
				APIName: "nico",
				Token:   "opts-token",
			},
			expected: expect{
				baseURL: "https://opts.example.com",
				org:     "opts-org",
				apiName: "nico",
				token:   "inbound",
			},
		},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			var cfg resolvedConfig
			err := cfg.FromCallConfig(c.in, c.req, c.opts)
			if c.expected.errContains != "" {
				require.ErrorContains(t, err, c.expected.errContains)
				return
			}
			require.NoError(t, err)
			require.Equal(t, c.expected.baseURL, cfg.BaseURL)
			require.Equal(t, c.expected.org, cfg.Org)
			require.Equal(t, c.expected.apiName, cfg.APIName)
			require.Equal(t, c.expected.token, cfg.Token)
		})
	}
}

// TestOptions_WithDefaults_PreservesCustomLogger asserts that
// withDefaults does not clobber a caller-supplied Log entry.
func TestOptions_WithDefaults(t *testing.T) {
	o := Options{}.withDefaults()
	require.Equal(t, "nico", o.APIName)
	require.NotNil(t, o.Log)
}

func requestWithBearer(token string) *mcp.CallToolRequest {
	return &mcp.CallToolRequest{
		Extra: &mcp.RequestExtra{
			Header: http.Header{"Authorization": []string{"Bearer " + token}},
		},
	}
}
