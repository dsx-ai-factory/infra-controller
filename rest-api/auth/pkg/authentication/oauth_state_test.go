// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authentication

import (
	"encoding/base64"
	"encoding/json"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
)

func TestKeycloakAuthService_OAuthState(t *testing.T) {
	service := &KeycloakAuthService{
		config: &config.KeycloakConfig{
			ClientSecret: "test-secret",
		},
	}
	redirectURI := "http://localhost:3000/callback"
	email := "john.doe@testorg.com"

	tests := []struct {
		name        string
		callbackURI string
		wantErr     bool
	}{
		{
			name:        "valid generated state",
			callbackURI: redirectURI,
			wantErr:     false,
		},
		{
			name:        "redirect URI mismatch",
			callbackURI: "http://evil.example/callback",
			wantErr:     true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			state, err := service.generateOAuthState(email, redirectURI)
			require.NoError(t, err)
			require.NotEmpty(t, state)

			err = service.ValidateOAuthState(state, tt.callbackURI)
			if tt.wantErr {
				assert.Error(t, err)
				assert.ErrorIs(t, err, ErrInvalidOAuthState)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestKeycloakAuthService_OAuthStateRejectsTamperedState(t *testing.T) {
	service := &KeycloakAuthService{
		config: &config.KeycloakConfig{
			ClientSecret: "test-secret",
		},
	}

	state, err := service.generateOAuthState("john.doe@testorg.com", "http://localhost:3000/callback")
	require.NoError(t, err)

	parts := strings.Split(state, ".")
	require.Len(t, parts, 2)

	tamperedPayload := []byte(`{"exp":9999999999,"nonce":"tampered","redirect_uri":"http://localhost:3000/callback","email":"attacker@evil.com"}`)
	tamperedState := base64.RawURLEncoding.EncodeToString(tamperedPayload) + "." + parts[1]

	err = service.ValidateOAuthState(tamperedState, "http://localhost:3000/callback")
	assert.Error(t, err)
	assert.ErrorIs(t, err, ErrInvalidOAuthState)
}

func TestKeycloakAuthService_OAuthStateRejectsExpiredState(t *testing.T) {
	secret := "test-secret"
	payload := oauthStatePayload{
		ExpiresAt:   time.Now().Add(-time.Minute).Unix(),
		Nonce:       "expired-nonce",
		RedirectURI: "http://localhost:3000/callback",
		Email:       "john.doe@testorg.com",
	}

	payloadBytes, err := json.Marshal(payload)
	require.NoError(t, err)

	signature := signOAuthStatePayload(secret, payloadBytes)
	state := base64.RawURLEncoding.EncodeToString(payloadBytes) + "." + base64.RawURLEncoding.EncodeToString(signature)

	service := &KeycloakAuthService{
		config: &config.KeycloakConfig{
			ClientSecret: secret,
		},
	}

	err = service.ValidateOAuthState(state, payload.RedirectURI)
	assert.Error(t, err)
	assert.ErrorIs(t, err, ErrInvalidOAuthState)
}

func TestKeycloakAuthService_InitiateAuthFlowIncludesOAuthState(t *testing.T) {
	testServer := CreateMockKeycloakServer(DefaultMockServerConfig())
	defer testServer.Close()

	service := NewKeycloakAuthService(&config.KeycloakConfig{
		BaseURL:         testServer.URL,
		ExternalBaseURL: testServer.URL,
		ClientID:        "test-client",
		ClientSecret:    "test-secret",
		Realm:           "nico",
	})

	redirectURI := "http://localhost:3000/callback"
	response, err := service.InitiateAuthFlow(t.Context(), "john.doe@testorg.com", redirectURI)
	require.NoError(t, err)
	require.NotEmpty(t, response.State)

	parsedURL, err := url.Parse(response.AuthURL)
	require.NoError(t, err)

	assert.Equal(t, response.State, parsedURL.Query().Get("state"))
	assert.NoError(t, service.ValidateOAuthState(response.State, redirectURI))
}
