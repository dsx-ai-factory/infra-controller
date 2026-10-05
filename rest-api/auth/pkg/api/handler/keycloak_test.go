// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cam "github.com/dsx-ai-factory/infra-controller/rest-api/auth/pkg/api/model"
	caa "github.com/dsx-ai-factory/infra-controller/rest-api/auth/pkg/authentication"
	"github.com/dsx-ai-factory/infra-controller/rest-api/auth/pkg/config"
)

var (
	// Mock responses for Keycloak endpoints
	mockAdminLoginResponse = `{"access_token":"admin-access-token","expires_in":300,"refresh_expires_in":1800,"refresh_token":"admin-refresh-token","token_type":"Bearer","not-before-policy":0,"session_state":"test-session-state","scope":"profile email"}`
	mockIDPsResponse       = `[{"alias":"testorg-idp","displayName":"TestOrg OIDC","providerId":"oidc","enabled":true,"config":{"kc.org.domain":"testorg.com"}},{"alias":"nvidia-idp","displayName":"NVIDIA OIDC","providerId":"oidc","enabled":true,"config":{"kc.org.domain":"nvidia.com"}},{"alias":"nico-idp","displayName":"NICo OIDC","providerId":"oidc","enabled":true,"config":{"kc.org.domain":"nico.nvidia.com"}}]`
	mockTokenResponse      = `{"access_token":"test-access-token","token_type":"Bearer","expires_in":3600}`
)

// createMockKeycloakServer creates a comprehensive mock Keycloak server for testing
func createMockKeycloakServer() *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		switch {
		case strings.Contains(req.URL.Path, "/protocol/openid-connect/token") && req.Method == "POST":
			handleMockTokenEndpoint(res, req)
		case strings.Contains(req.URL.Path, "/admin/realms/test-realm/identity-provider/instances"):
			handleMockIDPEndpoint(res, req)
		case strings.Contains(req.URL.Path, "/protocol/openid-connect/userinfo"):
			handleMockUserInfoEndpoint(res, req)
		case strings.Contains(req.URL.Path, "/protocol/openid-connect/logout"):
			handleMockLogoutEndpoint(res, req)
		case strings.Contains(req.URL.Path, "/realms/test-realm/protocol/openid-connect/certs"):
			// Mock JWKS endpoint
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(`{"keys":[{"kty":"RSA","e":"AQAB","use":"sig","kid":"test-key-id","n":"test-key-value"}]}`))
		default:
			res.WriteHeader(http.StatusNotFound)
		}
	}))
}

func handleMockTokenEndpoint(res http.ResponseWriter, req *http.Request) {
	err := req.ParseForm()
	if err != nil {
		res.WriteHeader(http.StatusBadRequest)
		return
	}

	grantType := req.Form.Get("grant_type")
	clientID := req.Form.Get("client_id")
	clientSecret := req.Form.Get("client_secret")

	// Check for Basic Auth if client_secret is empty
	if clientSecret == "" {
		username, password, ok := req.BasicAuth()
		if ok {
			clientID = username
			clientSecret = password
		}
	}

	// Debug logging (can be removed in production)
	// fmt.Printf("Mock server received: grant_type=%s, client_id=%s, client_secret=%s\n", grantType, clientID, clientSecret)

	switch grantType {
	case "password":
		// Admin login
		username := req.Form.Get("username")
		password := req.Form.Get("password")
		if username == "admin" && password == "admin-password" {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(mockAdminLoginResponse))
		} else {
			res.WriteHeader(http.StatusUnauthorized)
		}
	case "authorization_code":
		// Code exchange
		code := req.Form.Get("code")
		if code == "valid-auth-code" {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(mockTokenResponse))
		} else {
			res.WriteHeader(http.StatusBadRequest)
		}

	case "refresh_token":
		// Token refresh
		refreshToken := req.Form.Get("refresh_token")
		if refreshToken == "valid-refresh-token" {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(`{"access_token":"new-access-token","expires_in":3600,"refresh_expires_in":1800,"refresh_token":"new-refresh-token","token_type":"Bearer","scope":"openid profile email"}`))
		} else {
			res.WriteHeader(http.StatusBadRequest)
		}
	case "client_credentials":
		// Client credentials
		if (clientID == "service-client" && clientSecret == "service-secret") ||
			(clientID == "test-client" && clientSecret == "test-secret") {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			var response string
			if clientID == "service-client" {
				response = `{"access_token":"service-access-token","token_type":"Bearer","expires_in":3600}`
			} else {
				response = mockTokenResponse
			}
			// fmt.Printf("Mock server sending response: %s\n", response)
			res.Write([]byte(response))
		} else {
			res.WriteHeader(http.StatusUnauthorized)
		}
	default:
		res.WriteHeader(http.StatusBadRequest)
	}
}

func handleMockIDPEndpoint(res http.ResponseWriter, req *http.Request) {
	// Accept requests without proper auth headers for testing
	res.Header().Set("Content-Type", "application/json")
	res.WriteHeader(http.StatusOK)
	res.Write([]byte(mockIDPsResponse))
}

func handleMockUserInfoEndpoint(res http.ResponseWriter, req *http.Request) {
	authHeader := req.Header.Get("Authorization")
	if authHeader == "Bearer valid-access-token" {
		res.WriteHeader(http.StatusOK)
		res.Header().Set("Content-Type", "application/json")
		res.Write([]byte(`{"sub":"user-123","email":"john.doe@testorg.com","preferred_username":"john.doe"}`))
	} else {
		res.WriteHeader(http.StatusUnauthorized)
	}
}

func handleMockLogoutEndpoint(res http.ResponseWriter, req *http.Request) {
	err := req.ParseForm()
	if err != nil {
		res.WriteHeader(http.StatusBadRequest)
		return
	}

	refreshToken := req.Form.Get("refresh_token")
	if refreshToken == "valid-refresh-token" {
		res.WriteHeader(http.StatusOK)
	} else {
		res.WriteHeader(http.StatusBadRequest)
	}
}

func TestLoginHandler_Handle_EmailAuth(t *testing.T) {
	testServer := createMockKeycloakServer()
	defer testServer.Close()

	tests := []struct {
		name           string
		requestBody    cam.APILoginRequest
		expectedStatus int
		validateResp   func(*testing.T, *cam.APILoginResponse)
		wantErr        bool
	}{
		{
			name: "successful email authentication initiation",
			requestBody: cam.APILoginRequest{
				Email:       stringPtr("john.doe@testorg.com"),
				RedirectURI: stringPtr("http://localhost:3000/callback"),
			},
			expectedStatus: http.StatusOK,
			validateResp: func(t *testing.T, resp *cam.APILoginResponse) {
				assert.Contains(t, resp.AuthURL, "testorg-idp")
				assert.Contains(t, resp.AuthURL, "john.doe%40testorg.com")
				assert.Equal(t, "testorg-idp", resp.IDP)
				assert.Equal(t, "test-realm", resp.RealmName)
			},
			wantErr: false,
		},
		{
			name: "invalid email format",
			requestBody: cam.APILoginRequest{
				Email:       stringPtr("invalid-email"),
				RedirectURI: stringPtr("http://localhost:3000/callback"),
			},
			expectedStatus: http.StatusInternalServerError,
			validateResp:   nil,
			wantErr:        false, // Handler returns HTTP error, not Go error
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			keycloakConfig := &config.KeycloakConfig{
				BaseURL:         testServer.URL,
				ExternalBaseURL: testServer.URL,
				ClientID:        "test-client",
				ClientSecret:    "test-secret",
				Realm:           "test-realm",
			}

			keycloakAuth := caa.NewKeycloakAuthService(keycloakConfig)
			handler := NewLoginHandler(keycloakAuth)

			// Create request body
			reqBody, err := json.Marshal(tt.requestBody)
			require.NoError(t, err)

			// Create echo context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/auth/login", bytes.NewReader(reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			// Execute handler
			err = handler.Handle(c)

			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.expectedStatus, rec.Code)

				if tt.validateResp != nil {
					var response cam.APILoginResponse
					err = json.Unmarshal(rec.Body.Bytes(), &response)
					require.NoError(t, err)
					tt.validateResp(t, &response)
				}
			}
		})
	}
}

func TestLoginHandler_Handle_ClientCredentials(t *testing.T) {
	// Create mock Keycloak server for client credentials
	mockServerConfig := caa.MockKeycloakServerConfig{
		Responses: caa.GetDefaultMockResponses(),
		ValidCredentials: map[string]string{
			"admin": "admin-password",
		},
		ValidTokens: map[string]bool{
			"valid-access-token":   true,
			"service-access-token": true,
		},
		ValidCodes: map[string]bool{
			"valid-auth-code": true,
		},
	}

	mockServer := caa.CreateMockKeycloakServer(mockServerConfig)
	defer mockServer.Close()

	// Create real KeycloakAuthService with mock server
	keycloakConfig := &config.KeycloakConfig{
		BaseURL:               mockServer.URL,
		ExternalBaseURL:       mockServer.URL,
		ClientID:              "test-client",
		ClientSecret:          "test-secret",
		Realm:                 "nico",
		ServiceAccountEnabled: true,
	}

	keycloakAuth := caa.NewKeycloakAuthService(keycloakConfig)
	handler := NewLoginHandler(keycloakAuth)

	tests := []struct {
		name           string
		requestBody    cam.APILoginRequest
		expectedStatus int
		validateResp   func(*testing.T, *cam.APITokenResponse)
		wantErr        bool
	}{
		{
			name: "successful client credentials authentication",
			requestBody: cam.APILoginRequest{
				ClientID:     stringPtr("test-client"),
				ClientSecret: stringPtr("test-secret"),
			},
			expectedStatus: http.StatusOK,
			validateResp: func(t *testing.T, resp *cam.APITokenResponse) {
				assert.Equal(t, "service-access-token", resp.AccessToken) // Mock server returns this token
				assert.Equal(t, "Bearer", resp.TokenType)
				assert.Equal(t, 3600, resp.ExpiresIn)
				// Client credentials flow typically doesn't return refresh token
			},
			wantErr: false,
		},
		{
			name: "invalid client credentials",
			requestBody: cam.APILoginRequest{
				ClientID:     stringPtr("invalid-client"),
				ClientSecret: stringPtr("invalid-secret"),
			},
			expectedStatus: http.StatusUnauthorized,
			validateResp:   nil,
			wantErr:        false, // Handler returns HTTP error, not Go error
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create request
			reqBody, err := json.Marshal(tt.requestBody)
			require.NoError(t, err)

			req := httptest.NewRequest(http.MethodPost, "/login", bytes.NewReader(reqBody))
			req.Header.Set("Content-Type", "application/json")
			rec := httptest.NewRecorder()
			c := echo.New().NewContext(req, rec)

			// Execute handler
			err = handler.Handle(c)

			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
			assert.Equal(t, tt.expectedStatus, rec.Code)

			if tt.validateResp != nil && tt.expectedStatus == http.StatusOK {
				var response cam.APITokenResponse
				err = json.Unmarshal(rec.Body.Bytes(), &response)
				require.NoError(t, err)
				tt.validateResp(t, &response)
			}
		})
	}
}

func TestLoginHandler_Handle_ClientCredentials_ServiceAccountsDisabled(t *testing.T) {
	// Create mock Keycloak server
	mockServerConfig := caa.MockKeycloakServerConfig{
		Responses: caa.GetDefaultMockResponses(),
		ValidCredentials: map[string]string{
			"admin": "admin-password",
		},
		ValidTokens: map[string]bool{
			"valid-access-token":   true,
			"service-access-token": true,
		},
		ValidCodes: map[string]bool{
			"valid-auth-code": true,
		},
	}

	mockServer := caa.CreateMockKeycloakServer(mockServerConfig)
	defer mockServer.Close()

	// Create real KeycloakAuthService with service accounts disabled
	keycloakConfig := &config.KeycloakConfig{
		BaseURL:               mockServer.URL,
		ExternalBaseURL:       mockServer.URL,
		ClientID:              "test-client",
		ClientSecret:          "test-secret",
		Realm:                 "nico",
		ServiceAccountEnabled: false, // Service accounts disabled
	}

	keycloakAuth := caa.NewKeycloakAuthService(keycloakConfig)
	handler := NewLoginHandler(keycloakAuth)

	tests := []struct {
		name           string
		requestBody    cam.APILoginRequest
		expectedStatus int
		expectedMsg    string
		wantErr        bool
	}{
		{
			name: "client credentials request when service accounts disabled",
			requestBody: cam.APILoginRequest{
				ClientID:     stringPtr("test-client"),
				ClientSecret: stringPtr("test-secret"),
			},
			expectedStatus: http.StatusUnauthorized,
			expectedMsg:    "Service accounts are not enabled",
			wantErr:        false, // Handler returns HTTP error, not Go error
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create request
			reqBody, err := json.Marshal(tt.requestBody)
			require.NoError(t, err)

			req := httptest.NewRequest(http.MethodPost, "/login", bytes.NewReader(reqBody))
			req.Header.Set("Content-Type", "application/json")
			rec := httptest.NewRecorder()
			c := echo.New().NewContext(req, rec)

			// Execute handler
			err = handler.Handle(c)

			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
			assert.Equal(t, tt.expectedStatus, rec.Code)

			// Check error message contains expected text
			if tt.expectedMsg != "" {
				assert.Contains(t, rec.Body.String(), tt.expectedMsg)
			}
		})
	}
}

func TestLoginHandler_Handle_IDPAndDomainHandling(t *testing.T) {
	testServer := createMockKeycloakServer()
	defer testServer.Close()

	keycloakConfig := &config.KeycloakConfig{
		BaseURL:         testServer.URL,
		ExternalBaseURL: testServer.URL,
		ClientID:        "test-client",
		ClientSecret:    "test-secret",
		Realm:           "test-realm",
	}

	keycloakAuth := caa.NewKeycloakAuthService(keycloakConfig)
	handler := NewLoginHandler(keycloakAuth)

	tests := []struct {
		name           string
		requestBody    cam.APILoginRequest
		expectedStatus int
		validateResp   func(*testing.T, *cam.APILoginResponse)
		wantErr        bool
	}{
		{
			name: "successful email authentication with known domain (testorg.com)",
			requestBody: cam.APILoginRequest{
				Email:       stringPtr("john.doe@testorg.com"),
				RedirectURI: stringPtr("http://localhost:3000/callback"),
			},
			expectedStatus: http.StatusOK,
			validateResp: func(t *testing.T, resp *cam.APILoginResponse) {
				assert.Contains(t, resp.AuthURL, "testorg-idp")
				assert.Contains(t, resp.AuthURL, "john.doe%40testorg.com")
				assert.Equal(t, "testorg-idp", resp.IDP)
				assert.Equal(t, "test-realm", resp.RealmName)
				assert.Contains(t, resp.AuthURL, "redirect_uri=http%3A%2F%2Flocalhost%3A3000%2Fcallback")
			},
			wantErr: false,
		},
		{
			name: "successful email authentication with nvidia.com domain",
			requestBody: cam.APILoginRequest{
				Email:       stringPtr("alice.smith@nvidia.com"),
				RedirectURI: stringPtr("http://localhost:3000/nvidia-callback"),
			},
			expectedStatus: http.StatusOK,
			validateResp: func(t *testing.T, resp *cam.APILoginResponse) {
				assert.Contains(t, resp.AuthURL, "nvidia-idp")
				assert.Contains(t, resp.AuthURL, "alice.smith%40nvidia.com")
				assert.Equal(t, "nvidia-idp", resp.IDP)
				assert.Equal(t, "test-realm", resp.RealmName)
				assert.Contains(t, resp.AuthURL, "redirect_uri=http%3A%2F%2Flocalhost%3A3000%2Fnvidia-callback")
			},
			wantErr: false,
		},
		{
			name: "successful email authentication with nico.nvidia.com domain",
			requestBody: cam.APILoginRequest{
				Email:       stringPtr("developer@nico.nvidia.com"),
				RedirectURI: stringPtr("http://localhost:3000/nico-callback"),
			},
			expectedStatus: http.StatusOK,
			validateResp: func(t *testing.T, resp *cam.APILoginResponse) {
				assert.Contains(t, resp.AuthURL, "nico-idp")
				assert.Contains(t, resp.AuthURL, "developer%40nico.nvidia.com")
				assert.Equal(t, "nico-idp", resp.IDP)
				assert.Equal(t, "test-realm", resp.RealmName)
				assert.Contains(t, resp.AuthURL, "redirect_uri=http%3A%2F%2Flocalhost%3A3000%2Fnico-callback")
			},
			wantErr: false,
		},
		{
			name: "email authentication without redirect URI returns validation error",
			requestBody: cam.APILoginRequest{
				Email: stringPtr("user@testorg.com"),
				// RedirectURI is nil - should return validation error
			},
			expectedStatus: http.StatusBadRequest,
			validateResp:   nil,
			wantErr:        false, // Handler returns HTTP error, not Go error
		},
		{
			name: "unknown domain returns error",
			requestBody: cam.APILoginRequest{
				Email:       stringPtr("user@unknown-domain.com"),
				RedirectURI: stringPtr("http://localhost:3000/callback"),
			},
			expectedStatus: http.StatusInternalServerError,
			validateResp:   nil,
			wantErr:        false, // Handler returns HTTP error, not Go error
		},
		{
			name: "invalid email format returns error",
			requestBody: cam.APILoginRequest{
				Email:       stringPtr("invalid-email-format"),
				RedirectURI: stringPtr("http://localhost:3000/callback"),
			},
			expectedStatus: http.StatusInternalServerError,
			validateResp:   nil,
			wantErr:        false, // Handler returns HTTP error, not Go error
		},
		{
			name: "empty email returns validation error",
			requestBody: cam.APILoginRequest{
				Email:       stringPtr(""),
				RedirectURI: stringPtr("http://localhost:3000/callback"),
			},
			expectedStatus: http.StatusBadRequest,
			validateResp:   nil,
			wantErr:        false, // Handler returns HTTP error, not Go error
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create request
			reqBody, err := json.Marshal(tt.requestBody)
			require.NoError(t, err)

			req := httptest.NewRequest(http.MethodPost, "/login", bytes.NewReader(reqBody))
			req.Header.Set("Content-Type", "application/json")
			rec := httptest.NewRecorder()
			c := echo.New().NewContext(req, rec)

			// Execute handler
			err = handler.Handle(c)

			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
			assert.Equal(t, tt.expectedStatus, rec.Code)

			if tt.validateResp != nil && tt.expectedStatus == http.StatusOK {
				var response cam.APILoginResponse
				err = json.Unmarshal(rec.Body.Bytes(), &response)
				require.NoError(t, err)
				tt.validateResp(t, &response)
			}
		})
	}
}

func TestKeycloakAuthService_IDPListing(t *testing.T) {
	testServer := createMockKeycloakServer()
	defer testServer.Close()

	keycloakConfig := &config.KeycloakConfig{
		BaseURL:         testServer.URL,
		ExternalBaseURL: testServer.URL,
		ClientID:        "test-client",
		ClientSecret:    "test-secret",
		Realm:           "test-realm",
	}

	keycloakAuth := caa.NewKeycloakAuthService(keycloakConfig)

	tests := []struct {
		name        string
		email       string
		redirectURI string
		expectedIDP string
		expectError bool
	}{
		{
			name:        "testorg.com domain maps to testorg-idp",
			email:       "user@testorg.com",
			redirectURI: "http://localhost:3000/callback",
			expectedIDP: "testorg-idp",
			expectError: false,
		},
		{
			name:        "nvidia.com domain maps to nvidia-idp",
			email:       "employee@nvidia.com",
			redirectURI: "http://localhost:3000/callback",
			expectedIDP: "nvidia-idp",
			expectError: false,
		},
		{
			name:        "nico.nvidia.com domain maps to nico-idp",
			email:       "dev@nico.nvidia.com",
			redirectURI: "http://localhost:3000/callback",
			expectedIDP: "nico-idp",
			expectError: false,
		},
		{
			name:        "unknown domain returns error",
			email:       "user@unknown.com",
			redirectURI: "http://localhost:3000/callback",
			expectedIDP: "",
			expectError: true,
		},
		{
			name:        "malformed email returns error",
			email:       "not-an-email",
			redirectURI: "http://localhost:3000/callback",
			expectedIDP: "",
			expectError: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			response, err := keycloakAuth.InitiateAuthFlow(context.Background(), tt.email, tt.redirectURI)

			if tt.expectError {
				assert.Error(t, err)
				assert.Nil(t, response)
			} else {
				assert.NoError(t, err)
				assert.NotNil(t, response)
				assert.Equal(t, tt.expectedIDP, response.IDP)
				assert.Equal(t, "test-realm", response.RealmName)
				assert.Contains(t, response.AuthURL, tt.expectedIDP)
				// Check for URL-encoded redirect URI
				assert.Contains(t, response.AuthURL, "redirect_uri=http%3A%2F%2Flocalhost%3A3000%2Fcallback")
			}
		})
	}
}

func TestCallbackHandler_Handle(t *testing.T) {
	// Create mock Keycloak server for token exchange
	mockServerConfig := caa.MockKeycloakServerConfig{
		Responses: caa.GetDefaultMockResponses(),
		ValidCredentials: map[string]string{
			"admin": "admin-password",
		},
		ValidTokens: map[string]bool{
			"valid-access-token":   true,
			"service-access-token": true,
		},
		ValidCodes: map[string]bool{
			"valid-auth-code": true,
		},
	}

	mockServer := caa.CreateMockKeycloakServer(mockServerConfig)
	defer mockServer.Close()

	// Create real KeycloakAuthService with mock server
	keycloakConfig := &config.KeycloakConfig{
		BaseURL:               mockServer.URL,
		ExternalBaseURL:       mockServer.URL,
		ClientID:              "test-client",
		ClientSecret:          "test-secret",
		Realm:                 "nico",
		ServiceAccountEnabled: true,
	}

	keycloakAuth := caa.NewKeycloakAuthService(keycloakConfig)
	handler := NewCallbackHandler(keycloakAuth)

	tests := []struct {
		name           string
		requestBody    cam.APICallbackRequest
		expectedStatus int
		validateResp   func(*testing.T, *cam.APITokenResponse)
		wantErr        bool
	}{
		{
			name: "successful callback handling",
			requestBody: cam.APICallbackRequest{
				Code:        "valid-auth-code",
				RedirectURI: "http://localhost:3000/callback",
				State:       "state-123",
			},
			expectedStatus: http.StatusOK,
			validateResp: func(t *testing.T, resp *cam.APITokenResponse) {
				assert.Equal(t, "test-access-token", resp.AccessToken)
				assert.Equal(t, "test-refresh-token", resp.RefreshToken)
				assert.Equal(t, "Bearer", resp.TokenType)
				assert.Equal(t, 3600, resp.ExpiresIn)
			},
			wantErr: false,
		},
		{
			name: "invalid authorization code",
			requestBody: cam.APICallbackRequest{
				Code:        "invalid-code",
				RedirectURI: "http://localhost:3000/callback",
				State:       "state-123",
			},
			expectedStatus: http.StatusUnauthorized,
			validateResp:   nil,
			wantErr:        false, // Handler returns HTTP error, not Go error
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create request body
			reqBody, err := json.Marshal(tt.requestBody)
			require.NoError(t, err)

			// Create echo context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/auth/callback", bytes.NewReader(reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			// Execute handler
			err = handler.Handle(c)

			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
			assert.Equal(t, tt.expectedStatus, rec.Code)

			if tt.validateResp != nil && tt.expectedStatus == http.StatusOK {
				var response cam.APITokenResponse
				err = json.Unmarshal(rec.Body.Bytes(), &response)
				require.NoError(t, err)
				tt.validateResp(t, &response)
			}
		})
	}
}

// Helper function to create string pointers
func stringPtr(s string) *string {
	return &s
}
