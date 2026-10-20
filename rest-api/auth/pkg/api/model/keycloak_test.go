// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPICallbackRequest_Validate(t *testing.T) {
	validVerifier := strings.Repeat("a", 43)

	tests := []struct {
		name    string
		req     APICallbackRequest
		wantErr string
	}{
		{
			name: "valid request with RFC 7636 unreserved verifier characters",
			req: APICallbackRequest{
				Code:         "auth-code",
				RedirectURI:  "http://localhost:3000/callback",
				State:        "1792530000.nonce.mac",
				CodeVerifier: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij0123._~-",
			},
		},
		{
			name: "valid 128-character verifier",
			req: APICallbackRequest{
				Code:         "auth-code",
				RedirectURI:  "http://localhost:3000/callback",
				State:        "1792530000.nonce.mac",
				CodeVerifier: strings.Repeat("Z", 128),
			},
		},
		{
			name: "missing code",
			req: APICallbackRequest{
				RedirectURI:  "http://localhost:3000/callback",
				State:        "1792530000.nonce.mac",
				CodeVerifier: validVerifier,
			},
			wantErr: "code: code is required",
		},
		{
			name: "missing redirectUri",
			req: APICallbackRequest{
				Code:         "auth-code",
				State:        "1792530000.nonce.mac",
				CodeVerifier: validVerifier,
			},
			wantErr: "redirectUri: redirectUri is required",
		},
		{
			name: "invalid redirectUri",
			req: APICallbackRequest{
				Code:         "auth-code",
				RedirectURI:  "://bad-url",
				State:        "1792530000.nonce.mac",
				CodeVerifier: validVerifier,
			},
			wantErr: "redirectUri: redirectUri must be a valid URL",
		},
		{
			name: "missing state",
			req: APICallbackRequest{
				Code:         "auth-code",
				RedirectURI:  "http://localhost:3000/callback",
				CodeVerifier: validVerifier,
			},
			wantErr: "state: state is required",
		},
		{
			name: "oversized state rejected before signature verification",
			req: APICallbackRequest{
				Code:         "auth-code",
				RedirectURI:  "http://localhost:3000/callback",
				State:        strings.Repeat("s", MaxOAuthStateLength+1),
				CodeVerifier: validVerifier,
			},
			wantErr: "state: state must be at most 2048 bytes",
		},
		{
			name: "missing codeVerifier",
			req: APICallbackRequest{
				Code:        "auth-code",
				RedirectURI: "http://localhost:3000/callback",
				State:       "1792530000.nonce.mac",
			},
			wantErr: "codeVerifier: codeVerifier is required",
		},
		{
			name: "codeVerifier shorter than 43 bytes",
			req: APICallbackRequest{
				Code:         "auth-code",
				RedirectURI:  "http://localhost:3000/callback",
				State:        "1792530000.nonce.mac",
				CodeVerifier: strings.Repeat("a", 42),
			},
			wantErr: "codeVerifier: codeVerifier must be 43 to 128 characters",
		},
		{
			name: "codeVerifier longer than 128 bytes",
			req: APICallbackRequest{
				Code:         "auth-code",
				RedirectURI:  "http://localhost:3000/callback",
				State:        "1792530000.nonce.mac",
				CodeVerifier: strings.Repeat("a", 129),
			},
			wantErr: "codeVerifier: codeVerifier must be 43 to 128 characters",
		},
		{
			name: "multi-byte UTF-8 string of 43 runes rejected",
			req: APICallbackRequest{
				Code:         "auth-code",
				RedirectURI:  "http://localhost:3000/callback",
				State:        "1792530000.nonce.mac",
				CodeVerifier: strings.Repeat("é", 43),
			},
			wantErr: "codeVerifier: codeVerifier contains invalid characters",
		},
		{
			name: "reserved base64 and delimiter characters rejected",
			req: APICallbackRequest{
				Code:         "auth-code",
				RedirectURI:  "http://localhost:3000/callback",
				State:        "1792530000.nonce.mac",
				CodeVerifier: strings.Repeat("a", 42) + "+",
			},
			wantErr: "codeVerifier: codeVerifier contains invalid characters",
		},
		{
			name: "whitespace in codeVerifier rejected",
			req: APICallbackRequest{
				Code:         "auth-code",
				RedirectURI:  "http://localhost:3000/callback",
				State:        "1792530000.nonce.mac",
				CodeVerifier: strings.Repeat("a", 42) + " ",
			},
			wantErr: "codeVerifier: codeVerifier contains invalid characters",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				require.NoError(t, err)
			} else {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tt.wantErr)
			}
		})
	}
}
