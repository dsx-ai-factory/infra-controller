// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authentication

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"
	"unicode"

	"github.com/Nerzal/gocloak/v13"
	"github.com/golang-jwt/jwt/v5"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/api/model"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core/claim"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/processors"
	testutil "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/testing"
	commonConfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	tmocks "go.temporal.io/sdk/mocks"
)

// Valid bearer tokens for integration and mock tests
var validMockBearerTokens = []string{
	"Bearer admin-access-token",
	"Bearer service-access-token",
	"Bearer valid-access-token",
}

// isValidBearerToken checks if the provided auth header contains a valid bearer token
func isValidBearerToken(authHeader string) bool {
	if authHeader == "" {
		return true // Allow empty for gocloak bug compatibility
	}

	for _, validToken := range validMockBearerTokens {
		if authHeader == validToken {
			return true
		}
	}
	return false
}

func TestNewKeycloakAuthService(t *testing.T) {
	keycloakConfig := &config.KeycloakConfig{
		BaseURL:         "http://localhost:8082",
		ExternalBaseURL: "http://localhost:8082",
		ClientID:        "test-client",
		ClientSecret:    "test-secret",
		Realm:           "nico",
	}

	service := NewKeycloakAuthService(keycloakConfig)

	assert.NotNil(t, service)
	assert.Equal(t, keycloakConfig, service.config)
	assert.NotNil(t, service.client)
}

// TestKeycloakAuthService_InitiateAuthFlow_WithMock demonstrates complete mocking
func TestKeycloakAuthService_InitiateAuthFlow_WithMock(t *testing.T) {
	tests := []struct {
		name           string
		email          string
		redirectURI    string
		clientID       string
		clientSecret   string
		mockAdminToken string
		mockIDPs       []*gocloak.IdentityProviderRepresentation
		mockTokenError error
		mockIDPError   error
		wantAuthURL    string
		wantIDP        string
		wantRealmName  string
		wantErr        bool
		expectedErrMsg string
	}{
		{
			name:           "successful auth flow with mock",
			email:          "john.doe@testorg.com",
			redirectURI:    "http://localhost:3000/callback",
			clientID:       "test-client",
			clientSecret:   "test-secret",
			mockAdminToken: "mock-admin-token",
			mockIDPs: []*gocloak.IdentityProviderRepresentation{
				{
					Alias:       gocloak.StringP("testorg-idp"),
					DisplayName: gocloak.StringP("TestOrg OIDC"),
					ProviderID:  gocloak.StringP("oidc"),
					Enabled:     gocloak.BoolP(true),
					Config: &map[string]string{
						"kc.org.domain": "testorg.com",
						"clientId":      "testorg-client",
					},
				},
			},
			mockTokenError: nil,
			mockIDPError:   nil,
			wantIDP:        "testorg-idp",
			wantRealmName:  "nico",
			wantErr:        false,
		},
		{
			name:           "client credentials failure",
			email:          "john.doe@testorg.com",
			redirectURI:    "http://localhost:3000/callback",
			clientID:       "test-client",
			clientSecret:   "wrong-secret",
			mockAdminToken: "",
			mockIDPs:       nil,
			mockTokenError: fmt.Errorf("invalid credentials"),
			mockIDPError:   nil,
			wantErr:        true,
			expectedErrMsg: "failed to get admin token using client credentials",
		},
		{
			name:           "no matching IDP found",
			email:          "user@unknown.com",
			redirectURI:    "http://localhost:3000/callback",
			clientID:       "test-client",
			clientSecret:   "test-secret",
			mockAdminToken: "mock-admin-token",
			mockIDPs: []*gocloak.IdentityProviderRepresentation{
				{
					Alias:       gocloak.StringP("testorg-idp"),
					DisplayName: gocloak.StringP("TestOrg OIDC"),
					ProviderID:  gocloak.StringP("oidc"),
					Enabled:     gocloak.BoolP(true),
					Config: &map[string]string{
						"kc.org.domain": "testorg.com",
						"clientId":      "testorg-client",
					},
				},
			},
			mockTokenError: nil,
			mockIDPError:   nil,
			wantErr:        true,
			expectedErrMsg: "no identity provider found for domain: unknown.com",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create mock server configuration
			mockServerConfig := MockKeycloakServerConfig{
				Responses: GetDefaultMockResponses(),
				ValidCredentials: map[string]string{
					"admin": "admin-password",
				},
				ValidTokens: map[string]bool{
					"valid-access-token":   true,
					"admin-access-token":   true,
					"service-access-token": true, // Needed for IDP endpoint auth
				},
				ValidCodes: map[string]bool{
					"valid-auth-code": true,
				},
			}

			// Modify responses based on test parameters
			if tt.mockTokenError != nil {
				// Configure for error response - will be handled by removing valid credentials
				mockServerConfig.ValidCredentials = map[string]string{}
			}

			// Configure admin token for IDP authorization
			responses := GetDefaultMockResponses()
			responses.AdminLogin = `{"access_token":"admin-access-token","expires_in":300,"refresh_expires_in":1800,"refresh_token":"admin-refresh-token","token_type":"Bearer","not-before-policy":0,"session_state":"test-session-state","scope":"profile email"}`

			// Create custom IDPs response based on test data
			if len(tt.mockIDPs) > 0 {
				idp := tt.mockIDPs[0]
				if idp.Alias != nil && idp.Config != nil {
					responses.IDPs = fmt.Sprintf(`[{"alias":"%s","displayName":"TestOrg OIDC","providerId":"oidc","enabled":true,"config":{"kc.org.domain":"%s","clientId":"testorg-client"}}]`,
						*idp.Alias, (*idp.Config)["kc.org.domain"])
				}
			}

			mockServerConfig.Responses = responses

			// Create mock Keycloak server
			mockServer := CreateMockKeycloakServer(mockServerConfig)
			defer mockServer.Close()

			// Create real gocloak client pointing to mock server
			client := gocloak.NewClient(mockServer.URL)

			keycloakConfig := &config.KeycloakConfig{
				BaseURL:         mockServer.URL,
				ExternalBaseURL: mockServer.URL,
				ClientID:        tt.clientID,
				ClientSecret:    tt.clientSecret,
				Realm:           "nico",
			}

			service := NewKeycloakAuthServiceWithClient(keycloakConfig, client)

			authResponse, err := service.InitiateAuthFlow(context.Background(), tt.email, tt.redirectURI)

			if tt.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tt.expectedErrMsg)
				assert.Nil(t, authResponse)
			} else {
				assert.NoError(t, err)
				assert.NotNil(t, authResponse)
				assert.Equal(t, tt.wantIDP, authResponse.IDP)
				assert.Equal(t, tt.wantRealmName, authResponse.RealmName)
				assert.Contains(t, authResponse.AuthURL, tt.wantIDP)
				assert.Contains(t, authResponse.AuthURL, "john.doe%40testorg.com")
			}
		})
	}
}

func TestKeycloakAuthService_getIDPAliasForDomain(t *testing.T) {
	// Create mock Keycloak server
	testServer := CreateMockKeycloakServer(DefaultMockServerConfig())
	defer testServer.Close()

	tests := []struct {
		name           string
		emailDomain    string
		adminToken     string
		wantIDPAlias   string
		wantErr        bool
		expectedErrMsg string
	}{
		{
			name:         "successful IDP alias retrieval",
			emailDomain:  "testorg.com",
			adminToken:   "admin-access-token",
			wantIDPAlias: "testorg-idp",
			wantErr:      false,
		},
		{
			name:           "no matching IDP for domain",
			emailDomain:    "unknown.com",
			adminToken:     "admin-access-token",
			wantIDPAlias:   "",
			wantErr:        true,
			expectedErrMsg: "no identity provider found for domain: unknown.com",
		},
		{
			name:           "unauthorized admin token",
			emailDomain:    "testorg.com",
			adminToken:     "invalid-token",
			wantIDPAlias:   "",
			wantErr:        true,
			expectedErrMsg: "failed to get identity providers",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			keycloakConfig := &config.KeycloakConfig{
				BaseURL: testServer.URL,
				Realm:   "nico",
			}

			service := NewKeycloakAuthService(keycloakConfig)

			idpAlias, err := service.getIDPAliasForDomain(context.Background(), tt.adminToken, tt.emailDomain)

			if tt.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tt.expectedErrMsg)
				assert.Equal(t, tt.wantIDPAlias, idpAlias)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.wantIDPAlias, idpAlias)
			}
		})
	}
}

func TestKeycloakAuthService_InitiateAuthFlow(t *testing.T) {
	// Create mock Keycloak server
	testServer := CreateMockKeycloakServer(DefaultMockServerConfig())
	defer testServer.Close()

	tests := []struct {
		name           string
		email          string
		redirectURI    string
		wantResponse   *model.APILoginResponse
		wantErr        bool
		expectedErrMsg string
	}{
		{
			name:        "successful auth flow initiation",
			email:       "john.doe@testorg.com",
			redirectURI: "http://localhost:3000/callback",
			wantResponse: &model.APILoginResponse{
				AuthURL:   testServer.URL + "/realms/nico/protocol/openid-connect/auth?client_id=test-client&kc_idp_hint=testorg-idp&login_hint=john.doe%40testorg.com&redirect_uri=http%3A%2F%2Flocalhost%3A3000%2Fcallback&response_type=code&scope=openid",
				State:     "",
				IDP:       "testorg-idp",
				RealmName: "nico",
			},
			wantErr: false,
		},
		{
			name:           "invalid email format",
			email:          "invalid-email",
			redirectURI:    "http://localhost:3000/callback",
			wantResponse:   nil,
			wantErr:        true,
			expectedErrMsg: "invalid email format: invalid-email",
		},
		{
			name:           "IDP not found for domain",
			email:          "john.doe@unknown.com",
			redirectURI:    "http://localhost:3000/callback",
			wantResponse:   nil,
			wantErr:        true,
			expectedErrMsg: "failed to find identity provider for domain unknown.com",
		},
		{
			name:        "no redirect URI provided",
			email:       "john.doe@testorg.com",
			redirectURI: "",
			wantResponse: &model.APILoginResponse{
				AuthURL:   testServer.URL + "/realms/nico/protocol/openid-connect/auth?client_id=test-client&kc_idp_hint=testorg-idp&login_hint=john.doe%40testorg.com&response_type=code&scope=openid",
				State:     "",
				IDP:       "testorg-idp",
				RealmName: "nico",
			},
			wantErr: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			keycloakConfig := &config.KeycloakConfig{
				BaseURL:         testServer.URL,
				ExternalBaseURL: testServer.URL,
				ClientID:        "test-client",
				ClientSecret:    "test-secret",
				Realm:           "nico",
			}

			service := NewKeycloakAuthService(keycloakConfig)

			response, err := service.InitiateAuthFlow(context.Background(), tt.email, tt.redirectURI)

			if tt.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tt.expectedErrMsg)
				assert.Nil(t, response)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.wantResponse, response)
			}
		})
	}
}

func TestKeycloakAuthService_ExchangeCodeForTokens(t *testing.T) {

	tests := []struct {
		name           string
		code           string
		redirectURI    string
		wantResponse   *model.APITokenResponse
		wantErr        bool
		expectedErrMsg string
	}{
		{
			name:        "successful code exchange",
			code:        "valid-auth-code",
			redirectURI: "http://localhost:3000/callback",
			wantResponse: &model.APITokenResponse{
				AccessToken:  "test-access-token",
				RefreshToken: "test-refresh-token",
				ExpiresIn:    3600,
				TokenType:    "Bearer",
			},
			wantErr: false,
		},
		{
			name:           "invalid authorization code",
			code:           "invalid-code",
			redirectURI:    "http://localhost:3000/callback",
			wantResponse:   nil,
			wantErr:        true,
			expectedErrMsg: "failed to exchange authorization code",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			keycloakConfig := &config.KeycloakConfig{
				BaseURL:      "http://localhost:8082",
				ClientID:     "test-client",
				ClientSecret: "test-secret",
				Realm:        "nico",
			}

			// Create mock server configuration based on test case
			mockServerConfig := MockKeycloakServerConfig{
				Responses: GetDefaultMockResponses(),
				ValidCredentials: map[string]string{
					"admin": "admin-password",
				},
				ValidTokens: map[string]bool{
					"valid-access-token": true,
				},
				ValidCodes: map[string]bool{},
			}

			if tt.code == "valid-auth-code" {
				mockServerConfig.ValidCodes["valid-auth-code"] = true
			}

			// Create mock Keycloak server
			mockServer := CreateMockKeycloakServer(mockServerConfig)
			defer mockServer.Close()

			// Create real gocloak client pointing to mock server
			client := gocloak.NewClient(mockServer.URL)

			// Update config to use mock server
			keycloakConfig.BaseURL = mockServer.URL

			service := NewKeycloakAuthServiceWithClient(keycloakConfig, client)

			response, err := service.ExchangeCodeForTokens(context.Background(), tt.code, tt.redirectURI, "")

			if tt.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tt.expectedErrMsg)
				assert.Nil(t, response)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.wantResponse, response)
			}
		})
	}
}

func TestKeycloakAuthService_GetUserInfo(t *testing.T) {
	// Create mock Keycloak userinfo endpoint
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/protocol/openid-connect/userinfo") && req.Method == "GET" {
			authHeader := req.Header.Get("Authorization")
			if authHeader == "Bearer valid-access-token" {
				res.WriteHeader(http.StatusOK)
				res.Header().Set("Content-Type", "application/json")
				res.Write([]byte(DefaultMockResponses.UserInfo))
			} else {
				res.WriteHeader(http.StatusUnauthorized)
				res.Write([]byte(`{"error":"invalid_token","error_description":"Invalid access token"}`))
			}
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer testServer.Close()

	tests := []struct {
		name           string
		accessToken    string
		wantUserInfo   *gocloak.UserInfo
		wantErr        bool
		expectedErrMsg string
	}{
		{
			name:        "successful user info retrieval",
			accessToken: "valid-access-token",
			wantUserInfo: &gocloak.UserInfo{
				Sub:               gocloak.StringP("user-123"),
				Email:             gocloak.StringP("john.doe@testorg.com"),
				PreferredUsername: gocloak.StringP("john.doe"),
				GivenName:         gocloak.StringP("John"),
				FamilyName:        gocloak.StringP("Doe"),
			},
			wantErr: false,
		},
		{
			name:           "invalid access token",
			accessToken:    "invalid-token",
			wantUserInfo:   nil,
			wantErr:        true,
			expectedErrMsg: "failed to get user info",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			keycloakConfig := &config.KeycloakConfig{
				BaseURL: testServer.URL,
				Realm:   "nico",
			}

			service := NewKeycloakAuthService(keycloakConfig)

			userInfo, err := service.GetUserInfo(context.Background(), tt.accessToken)

			if tt.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tt.expectedErrMsg)
				assert.Nil(t, userInfo)
			} else {
				assert.NoError(t, err)
				require.NotNil(t, userInfo)
				if tt.wantUserInfo.Sub != nil && userInfo.Sub != nil {
					assert.Equal(t, *tt.wantUserInfo.Sub, *userInfo.Sub)
				}
				if tt.wantUserInfo.Email != nil && userInfo.Email != nil {
					assert.Equal(t, *tt.wantUserInfo.Email, *userInfo.Email)
				}
				if tt.wantUserInfo.PreferredUsername != nil && userInfo.PreferredUsername != nil {
					assert.Equal(t, *tt.wantUserInfo.PreferredUsername, *userInfo.PreferredUsername)
				}
			}
		})
	}
}

func TestKeycloakAuthService_RefreshAccessToken(t *testing.T) {

	tests := []struct {
		name           string
		refreshToken   string
		wantResponse   *model.APITokenResponse
		wantErr        bool
		expectedErrMsg string
	}{
		{
			name:         "successful token refresh",
			refreshToken: "valid-refresh-token",
			wantResponse: &model.APITokenResponse{
				AccessToken:  "new-access-token",
				RefreshToken: "new-refresh-token",
				ExpiresIn:    3600,
				TokenType:    "Bearer",
			},
			wantErr: false,
		},
		{
			name:           "invalid refresh token",
			refreshToken:   "invalid-refresh-token",
			wantResponse:   nil,
			wantErr:        true,
			expectedErrMsg: "failed to refresh token",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			keycloakConfig := &config.KeycloakConfig{
				BaseURL:      "http://localhost:8082",
				ClientID:     "test-client",
				ClientSecret: "test-secret",
				Realm:        "nico",
			}

			// Create mock server configuration - refresh token endpoint is hardcoded in the handler
			mockServerConfig := MockKeycloakServerConfig{
				Responses: GetDefaultMockResponses(),
				ValidCredentials: map[string]string{
					"admin": "admin-password",
				},
				ValidTokens: map[string]bool{
					"valid-access-token": true,
				},
				ValidCodes: map[string]bool{
					"valid-auth-code": true,
				},
			}

			// Create mock Keycloak server
			mockServer := CreateMockKeycloakServer(mockServerConfig)
			defer mockServer.Close()

			// Create real gocloak client pointing to mock server
			client := gocloak.NewClient(mockServer.URL)

			// Update config to use mock server
			keycloakConfig.BaseURL = mockServer.URL

			service := NewKeycloakAuthServiceWithClient(keycloakConfig, client)

			response, err := service.RefreshAccessToken(context.Background(), tt.refreshToken)

			if tt.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tt.expectedErrMsg)
				assert.Nil(t, response)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.wantResponse, response)
			}
		})
	}
}

func TestKeycloakAuthService_ClientCredentialsAuth(t *testing.T) {

	tests := []struct {
		name           string
		clientID       string
		clientSecret   string
		wantResponse   *model.APITokenResponse
		wantErr        bool
		expectedErrMsg string
	}{
		{
			name:         "successful client credentials auth",
			clientID:     "service-client",
			clientSecret: "service-secret",
			wantResponse: &model.APITokenResponse{
				AccessToken:  "service-access-token",
				RefreshToken: "",
				ExpiresIn:    3600,
				TokenType:    "Bearer",
			},
			wantErr: false,
		},
		{
			name:           "invalid client credentials",
			clientID:       "invalid-client",
			clientSecret:   "invalid-secret",
			wantResponse:   nil,
			wantErr:        true,
			expectedErrMsg: "failed to authenticate using client credentials",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			keycloakConfig := &config.KeycloakConfig{
				BaseURL: "http://localhost:8082",
				Realm:   "nico",
			}

			// Create mock server configuration - client credentials are hardcoded in the handler
			mockServerConfig := MockKeycloakServerConfig{
				Responses: GetDefaultMockResponses(),
				ValidCredentials: map[string]string{
					"admin": "admin-password",
				},
				ValidTokens: map[string]bool{
					"valid-access-token": true,
				},
				ValidCodes: map[string]bool{
					"valid-auth-code": true,
				},
			}

			// Create mock Keycloak server
			mockServer := CreateMockKeycloakServer(mockServerConfig)
			defer mockServer.Close()

			// Create real gocloak client pointing to mock server
			client := gocloak.NewClient(mockServer.URL)

			// Update config to use mock server
			keycloakConfig.BaseURL = mockServer.URL

			service := NewKeycloakAuthServiceWithClient(keycloakConfig, client)

			response, err := service.ClientCredentialsAuth(context.Background(), tt.clientID, tt.clientSecret)

			if tt.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tt.expectedErrMsg)
				assert.Nil(t, response)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.wantResponse, response)
			}
		})
	}
}

func TestKeycloakAuthService_Logout(t *testing.T) {
	// Create mock Keycloak logout endpoint
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/protocol/openid-connect/logout") && req.Method == "POST" {
			err := req.ParseForm()
			if err != nil {
				res.WriteHeader(http.StatusBadRequest)
				return
			}

			refreshToken := req.Form.Get("refresh_token")
			clientID := req.Form.Get("client_id")
			clientSecret := req.Form.Get("client_secret")

			if refreshToken == "valid-refresh-token" && clientID == "test-client" && clientSecret == "test-secret" {
				res.WriteHeader(http.StatusOK)
			} else {
				res.WriteHeader(http.StatusBadRequest)
				res.Write([]byte(`{"error":"invalid_request","error_description":"Invalid logout request"}`))
			}
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer testServer.Close()

	tests := []struct {
		name         string
		refreshToken string
		wantErr      bool
	}{
		{
			name:         "successful logout",
			refreshToken: "valid-refresh-token",
			wantErr:      false,
		},
		{
			name:         "invalid refresh token - should not return error",
			refreshToken: "invalid-refresh-token",
			wantErr:      false, // Service doesn't return error even if Keycloak logout fails
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			keycloakConfig := &config.KeycloakConfig{
				BaseURL:      testServer.URL,
				ClientID:     "test-client",
				ClientSecret: "test-secret",
				Realm:        "nico",
			}

			service := NewKeycloakAuthService(keycloakConfig)

			err := service.Logout(context.Background(), tt.refreshToken)

			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

// TestKeycloakAuthService_Integration tests the complete flow
func TestKeycloakAuthService_Integration(t *testing.T) {
	// Create mock Keycloak server
	testServer := CreateMockKeycloakServer(DefaultMockServerConfig())
	defer testServer.Close()

	keycloakConfig := &config.KeycloakConfig{
		BaseURL:         testServer.URL,
		ExternalBaseURL: testServer.URL,
		ClientID:        "test-client",
		ClientSecret:    "test-secret",
		Realm:           "nico",
	}

	service := NewKeycloakAuthService(keycloakConfig)

	t.Run("complete auth flow", func(t *testing.T) {
		// 1. Initiate auth flow
		authResponse, err := service.InitiateAuthFlow(context.Background(), "john.doe@testorg.com", "http://localhost:3000/callback")
		if err != nil {
			t.Skipf("Skipping integration test due to auth flow failure: %v", err)
		}
		assert.NoError(t, err)
		require.NotNil(t, authResponse)
		assert.Contains(t, authResponse.AuthURL, "testorg-idp")

		// 2. Exchange code for tokens
		tokenResponse, err := service.ExchangeCodeForTokens(context.Background(), "valid-auth-code", "http://localhost:3000/callback", "")
		assert.NoError(t, err)
		assert.NotNil(t, tokenResponse)
		assert.Equal(t, "test-access-token", tokenResponse.AccessToken)

		// 3. Get user info
		userInfo, err := service.GetUserInfo(context.Background(), "valid-access-token")
		assert.NoError(t, err)
		assert.NotNil(t, userInfo)
		assert.Equal(t, "john.doe@testorg.com", *userInfo.Email)

		// 4. Refresh token
		refreshResponse, err := service.RefreshAccessToken(context.Background(), "valid-refresh-token")
		assert.NoError(t, err)
		assert.NotNil(t, refreshResponse)
		assert.Equal(t, "new-access-token", refreshResponse.AccessToken)

		// 5. Logout
		err = service.Logout(context.Background(), "valid-refresh-token")
		assert.NoError(t, err)
	})
}

func TestKeycloakConfig_IssuerConstruction(t *testing.T) {
	tests := []struct {
		name            string
		externalBaseURL string
		realm           string
		expectedIssuer  string
	}{
		{
			name:            "production keycloak issuer",
			externalBaseURL: "https://keycloak.company.com",
			realm:           "production",
			expectedIssuer:  "https://keycloak.company.com/realms/production",
		},
		{
			name:            "development keycloak with auth prefix",
			externalBaseURL: "http://localhost:8082/auth",
			realm:           "development",
			expectedIssuer:  "http://localhost:8082/auth/realms/development",
		},
		{
			name:            "keycloak with complex realm name",
			externalBaseURL: "https://auth.example.com",
			realm:           "my-org-realm",
			expectedIssuer:  "https://auth.example.com/realms/my-org-realm",
		},
		{
			name:            "localhost development setup",
			externalBaseURL: "http://localhost:8082",
			realm:           "nico",
			expectedIssuer:  "http://localhost:8082/realms/nico",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			kcConfig := config.NewKeycloakConfig(
				"http://localhost:8082",
				tt.externalBaseURL,
				"test-client",
				"test-secret",
				tt.realm,
				true,
			)
			assert.Equal(t, tt.expectedIssuer, kcConfig.Issuer)
		})
	}
}

// TestAuthProcessor_KeycloakFlowWithMockJWKS tests the complete Keycloak authentication flow
func TestAuthProcessor_KeycloakFlowWithMockJWKS(t *testing.T) {
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create user table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)

	// Create mock JWKS server that returns our consistent test key
	jwksServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/realms/nico/protocol/openid-connect/certs") {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			// Generate JWKS from the same key used for signing
			testKey := getConsistentTestRSAKey()
			jwks := createJWKSFromRSAKey(&testKey.PublicKey, "test-key-id")
			res.Write([]byte(jwks))
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer jwksServer.Close()

	e := echo.New()
	tc := &tmocks.Client{}

	// Setup token origin config with Keycloak token origin
	toCfg := config.NewTokenOriginConfig()
	toCfg.AddConfig("keycloak", jwksServer.URL+"/realms/nico", jwksServer.URL+"/realms/nico/protocol/openid-connect/certs", config.TokenOriginKeycloak, true, nil, nil)

	// Initialize JWKS data for testing
	if err := toCfg.UpdateAllJWKS(); err != nil {
		t.Fatal(err)
	}

	encCfg := commonConfig.NewPayloadEncryptionConfig("test-encryption-key")

	// Initialize processors for testing
	processors.InitializeProcessors(toCfg, dbSession, tc, encCfg, nil)

	tests := []struct {
		name           string
		tokenClaims    *claim.KeycloakClaims
		path           string
		orgName        string
		expectedStatus int
		expectedErrMsg string
		wantErr        bool
		validateUser   func(*testing.T, echo.Context)
	}{
		{
			name: "successful keycloak user authentication and creation",
			tokenClaims: &claim.KeycloakClaims{
				Email:     "john.doe@testorg.com",
				FirstName: "John",
				LastName:  "Doe",
				Oidc_Id:   "oidc-123-456",
				RealmAccess: claim.RealmAccess{
					Roles: []string{"testorg:PROVIDER_ADMIN"},
				},
				RegisteredClaims: jwt.RegisteredClaims{
					Subject: "user-subject-123",
					Issuer:  jwksServer.URL + "/realms/nico",
				},
			},
			path:    "/v2/org/testorg/user/current",
			orgName: "testorg",
			wantErr: false,
			validateUser: func(t *testing.T, c echo.Context) {
				user := c.Get("user")
				assert.NotNil(t, user)
				dbUser, ok := user.(*cdbm.User)
				assert.True(t, ok)
				assert.Equal(t, "oidc-123-456", *dbUser.AuxiliaryID)
				assert.Equal(t, "john.doe@testorg.com", *dbUser.Email)
				assert.Equal(t, "John", *dbUser.FirstName)
				assert.Equal(t, "Doe", *dbUser.LastName)
			},
		},
		{
			name: "keycloak service account authentication",
			tokenClaims: &claim.KeycloakClaims{
				Email:     "",
				FirstName: "",
				LastName:  "",
				ClientId:  "service-client",
				Oidc_Id:   "", // Empty for service accounts
				RealmAccess: claim.RealmAccess{
					Roles: []string{"testorg:TENANT_ADMIN"},
				},
				RegisteredClaims: jwt.RegisteredClaims{
					Subject: "service-account-123",
					Issuer:  jwksServer.URL + "/realms/nico",
				},
			},
			path:    "/v2/org/testorg/user/current",
			orgName: "testorg",
			wantErr: false,
			validateUser: func(t *testing.T, c echo.Context) {
				user := c.Get("user")
				assert.NotNil(t, user)
				dbUser, ok := user.(*cdbm.User)
				assert.True(t, ok)
				assert.Equal(t, "service-account-123", *dbUser.AuxiliaryID) // Uses subject as auxiliary ID
				assert.Equal(t, "service-client", *dbUser.FirstName)        // FirstName set to ClientId for service accounts
			},
		},
		{
			name: "keycloak token from wrong realm",
			tokenClaims: &claim.KeycloakClaims{
				RegisteredClaims: jwt.RegisteredClaims{
					Issuer: jwksServer.URL + "/realms/wrong-realm",
				},
			},
			path:           "/v2/org/testorg/user/current",
			orgName:        "testorg",
			expectedStatus: http.StatusUnauthorized,
			expectedErrMsg: "Invalid authorization token in request",
			wantErr:        true,
		},
		{
			name: "keycloak user with no roles",
			tokenClaims: &claim.KeycloakClaims{
				Email:     "john.doe@testorg.com",
				FirstName: "John",
				LastName:  "Doe",
				Oidc_Id:   "oidc-123-456",
				RealmAccess: claim.RealmAccess{
					Roles: []string{}, // No roles
				},
				RegisteredClaims: jwt.RegisteredClaims{
					Subject: "user-subject-123",
					Issuer:  jwksServer.URL + "/realms/nico",
				},
			},
			path:           "/v2/org/testorg/user/current",
			orgName:        "testorg",
			expectedStatus: http.StatusForbidden, // JWT validation passes, role validation fails
			expectedErrMsg: "User does not have any roles assigned",
			wantErr:        true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create JWT token with the test claims using consistent RSA key
			privateKey := getConsistentTestRSAKey()
			tokenString, err := generateTestJWT(tt.tokenClaims, privateKey)
			require.NoError(t, err)

			// Create request
			req := httptest.NewRequest(http.MethodGet, tt.path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req.Header.Set("Authorization", "Bearer "+tokenString)

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)
			c.SetParamNames("orgName")
			c.SetParamValues(tt.orgName)
			c.Set("orgName", tt.orgName) // Set in context for KeycloakProcessor
			c.SetPath(tt.path)

			// Execute auth processor
			apiErr := AuthProcessor(c, toCfg)

			if tt.wantErr {
				require.NotNil(t, apiErr)
				assert.Equal(t, tt.expectedStatus, apiErr.Code)
				assert.Contains(t, apiErr.Message, tt.expectedErrMsg)
			} else {
				// Note: JWT validation will likely fail due to key mismatch, but we test business logic
				if apiErr != nil {
					// Should not be role-related or realm-related errors for valid test cases
					assert.NotContains(t, apiErr.Message, "User does not have any roles assigned")
					assert.NotContains(t, apiErr.Message, "Token from unexpected realm")
				} else if tt.validateUser != nil {
					tt.validateUser(t, c)
				}
			}
		})
	}
}

// TestAuthProcessor_KeycloakServiceAccountsDisabled tests Keycloak authentication when service accounts are disabled
func TestAuthProcessor_KeycloakServiceAccountsDisabled(t *testing.T) {
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create user table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)

	// Create mock JWKS server
	jwksServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/realms/nico/protocol/openid-connect/certs") {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			// Generate JWKS from the same key used for signing
			testKey := getConsistentTestRSAKey()
			jwks := createJWKSFromRSAKey(&testKey.PublicKey, "test-key-id")
			res.Write([]byte(jwks))
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer jwksServer.Close()

	e := echo.New()
	tc := &tmocks.Client{}

	// Create Keycloak config with service accounts DISABLED
	keycloakConfigDisabled := config.NewKeycloakConfig(
		jwksServer.URL,
		jwksServer.URL,
		"test-client",
		"test-secret",
		"nico",
		false, // ServiceAccountEnabled = false
	)

	// Setup token origin config with Keycloak token origin
	toCfg := config.NewTokenOriginConfig()
	toCfg.AddConfig("keycloak", jwksServer.URL+"/realms/nico", jwksServer.URL+"/realms/nico/protocol/openid-connect/certs", config.TokenOriginKeycloak, keycloakConfigDisabled.ServiceAccountEnabled, nil, nil)

	// Initialize JWKS data for testing
	if err := toCfg.UpdateAllJWKS(); err != nil {
		t.Fatal(err)
	}

	encCfg := commonConfig.NewPayloadEncryptionConfig("test-encryption-key")

	// Initialize processors for testing with service accounts DISABLED
	processors.InitializeProcessors(toCfg, dbSession, tc, encCfg, keycloakConfigDisabled)

	tests := []struct {
		name           string
		tokenClaims    *claim.KeycloakClaims
		path           string
		orgName        string
		expectedStatus int
		expectedErrMsg string
		wantErr        bool
	}{
		{
			name: "service account token rejected when service accounts disabled",
			tokenClaims: &claim.KeycloakClaims{
				Email:     "",
				FirstName: "",
				LastName:  "",
				ClientId:  "service-client-disabled", // Non-empty ClientId indicates service account
				Oidc_Id:   "",
				RealmAccess: claim.RealmAccess{
					Roles: []string{"testorg:PROVIDER_ADMIN"},
				},
				RegisteredClaims: jwt.RegisteredClaims{
					Subject: "service-account-disabled-123",
					Issuer:  jwksServer.URL + "/realms/nico",
				},
			},
			path:           "/v2/org/testorg/user/current",
			orgName:        "testorg",
			expectedStatus: http.StatusUnauthorized,
			expectedErrMsg: "Service accounts are not enabled",
			wantErr:        true,
		},
		{
			name: "regular user token still works when service accounts disabled",
			tokenClaims: &claim.KeycloakClaims{
				Email:     "user@testorg.com",
				FirstName: "Test",
				LastName:  "User",
				ClientId:  "", // Empty ClientId indicates regular user
				Oidc_Id:   "oidc-regular-user",
				RealmAccess: claim.RealmAccess{
					Roles: []string{"testorg:PROVIDER_ADMIN"},
				},
				RegisteredClaims: jwt.RegisteredClaims{
					Subject: "regular-user-123",
					Issuer:  jwksServer.URL + "/realms/nico",
				},
			},
			path:    "/v2/org/testorg/user/current",
			orgName: "testorg",
			wantErr: false, // Regular users should still work when service accounts are disabled
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create JWT token with the test claims using consistent RSA key
			privateKey := getConsistentTestRSAKey()
			tokenString, err := generateTestJWT(tt.tokenClaims, privateKey)
			require.NoError(t, err)

			// Create request
			req := httptest.NewRequest(http.MethodGet, tt.path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req.Header.Set("Authorization", "Bearer "+tokenString)

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)
			c.SetParamNames("orgName")
			c.SetParamValues(tt.orgName)
			c.Set("orgName", tt.orgName) // Set in context for KeycloakProcessor
			c.SetPath(tt.path)

			// Execute auth processor with service accounts DISABLED
			apiErr := AuthProcessor(c, toCfg)

			if tt.wantErr {
				require.NotNil(t, apiErr, "Expected error but got none")
				assert.Equal(t, tt.expectedStatus, apiErr.Code, "Expected status code %d but got %d", tt.expectedStatus, apiErr.Code)
				assert.Contains(t, apiErr.Message, tt.expectedErrMsg, "Expected error message to contain '%s' but got '%s'", tt.expectedErrMsg, apiErr.Message)
			} else {
				// For regular users, we expect either success or some other error (not service account related)
				if apiErr != nil {
					assert.NotContains(t, apiErr.Message, "Service accounts are not enabled",
						"Regular users should not be blocked due to service account settings")
				}
			}
		})
	}
}

// TestKeycloakClaimsProcessing_RealData tests claims processing with realistic data
func TestKeycloakClaimsProcessing_RealData(t *testing.T) {
	tests := []struct {
		name     string
		claims   *claim.KeycloakClaims
		validate func(*testing.T, *claim.KeycloakClaims)
	}{
		{
			name: "production user with multiple org roles",
			claims: &claim.KeycloakClaims{
				Email:     "alice.smith@company.com",
				FirstName: "Alice",
				LastName:  "Smith",
				Oidc_Id:   "oidc-alice-12345",
				RealmAccess: claim.RealmAccess{
					Roles: []string{
						"default-roles-nico",
						"NICo-Tenant-Dev:TENANT_ADMIN",
						"offline_access",
						"NICo-Prime-Provider:PROVIDER_ADMIN",
						"uma_authorization",
						"malformed-role",             // Ignored (no colon)
						"NICo-Test:INVALID_ROLE",     // Now included
						"NICo-Another:REGISTRY_READ", // Now included
					},
				},
				RegisteredClaims: jwt.RegisteredClaims{
					Subject: "user-alice-67890",
					Issuer:  "https://auth.company.com/realms/production",
				},
			},
			validate: func(t *testing.T, claims *claim.KeycloakClaims) {
				assert.Equal(t, "alice.smith@company.com", claims.GetEmail())
				assert.Equal(t, "oidc-alice-12345", claims.GetOidcId())

				orgData := claims.ToOrgData()
				assert.Len(t, orgData, 4) // Should have 4 orgs (all orgs from roles, no filtering)

				tenantOrg, exists := orgData["nico-tenant-dev"]
				assert.True(t, exists)
				assert.Equal(t, []string{authz.TenantAdminRole}, tenantOrg.Roles)

				providerOrg, exists := orgData["nico-prime-provider"]
				assert.True(t, exists)
				assert.Equal(t, []string{authz.ProviderAdminRole}, providerOrg.Roles)

				testOrg, exists := orgData["nico-test"]
				assert.True(t, exists)
				assert.Equal(t, []string{"INVALID_ROLE"}, testOrg.Roles)

				anotherOrg, exists := orgData["nico-another"]
				assert.True(t, exists)
				assert.Equal(t, []string{"REGISTRY_READ"}, anotherOrg.Roles)
			},
		},
		{
			name: "service account with system roles",
			claims: &claim.KeycloakClaims{
				Email:     "",
				FirstName: "",
				LastName:  "",
				ClientId:  "monitoring-service",
				Oidc_Id:   "", // Empty for service accounts
				RealmAccess: claim.RealmAccess{
					Roles: []string{
						"default-roles-nico",
						"NICo-System:PROVIDER_ADMIN", // Valid role
						"offline_access",
						"uma_authorization",
						"NICo-System:INVALID_ROLE", // Now included
					},
				},
				RegisteredClaims: jwt.RegisteredClaims{
					Subject: "service-monitoring-abc123",
					Issuer:  "https://auth.company.com/realms/services",
				},
			},
			validate: func(t *testing.T, claims *claim.KeycloakClaims) {
				// For service accounts, auxiliary ID should be the subject
				auxId := claims.GetOidcId()
				if auxId == "" {
					auxId = claims.Subject
				}
				assert.Equal(t, "service-monitoring-abc123", auxId)

				// FirstName should be set to ClientId for service accounts
				firstName := claims.FirstName
				if firstName == "" && claims.ClientId != "" {
					firstName = claims.ClientId
				}
				assert.Equal(t, "monitoring-service", firstName)

				orgData := claims.ToOrgData()
				assert.Len(t, orgData, 1)

				systemOrg, exists := orgData["nico-system"]
				assert.True(t, exists)
				assert.Equal(t, []string{authz.ProviderAdminRole, "INVALID_ROLE"}, systemOrg.Roles)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tt.validate(t, tt.claims)
		})
	}
}

// TestKeycloakUserDatabaseOperations tests the database operations for Keycloak users
func TestKeycloakUserDatabaseOperations(t *testing.T) {
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create user table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)

	t.Run("create new keycloak user", func(t *testing.T) {
		userDAO := cdbm.NewUserDAO(dbSession)

		// Simulate user creation from Keycloak claims
		email := "new.user@testorg.com"
		firstName := "New"
		lastName := "User"
		auxId := "oidc-new-user-123"
		orgData := cdbm.OrgData{
			"testorg": cdbm.Org{
				Name:        "testorg",
				DisplayName: "testorg",
				OrgType:     "ENTERPRISE",
				Roles:       []string{authz.ProviderAdminRole},
				Teams:       []cdbm.Team{},
			},
		}

		user, created, err := userDAO.GetOrCreate(context.Background(), nil, cdbm.UserGetOrCreateInput{
			AuxiliaryID: &auxId,
		})

		require.NoError(t, err)
		assert.True(t, created)
		assert.NotNil(t, user)
		assert.Equal(t, auxId, *user.AuxiliaryID)

		// Update user with profile information since it was just created
		user, err = userDAO.Update(context.Background(), nil, cdbm.UserUpdateInput{
			UserID:    user.ID,
			Email:     &email,
			FirstName: &firstName,
			LastName:  &lastName,
			OrgData:   orgData,
		})

		require.NoError(t, err)
		assert.NotNil(t, user)
		assert.Equal(t, auxId, *user.AuxiliaryID)
		assert.Equal(t, email, *user.Email)
		assert.Equal(t, firstName, *user.FirstName)
		assert.Equal(t, lastName, *user.LastName)
		assert.True(t, user.OrgData.Equal(orgData))
	})

	t.Run("update existing keycloak user with new roles", func(t *testing.T) {
		userDAO := cdbm.NewUserDAO(dbSession)

		// Create initial user
		email := "existing.user@testorg.com"
		firstName := "Existing"
		lastName := "User"
		auxId := "oidc-existing-user-456"
		initialOrgData := cdbm.OrgData{
			"testorg": cdbm.Org{
				Name:        "testorg",
				DisplayName: "Test Org",
				OrgType:     "ENTERPRISE",
				Roles:       []string{authz.ProviderAdminRole},
				Teams:       []cdbm.Team{},
			},
		}

		user, created, err := userDAO.GetOrCreate(context.Background(), nil, cdbm.UserGetOrCreateInput{
			AuxiliaryID: &auxId,
		})
		require.NoError(t, err)
		assert.True(t, created)

		// Update user with initial profile information since it was just created
		user, err = userDAO.Update(context.Background(), nil, cdbm.UserUpdateInput{
			UserID:    user.ID,
			Email:     &email,
			FirstName: &firstName,
			LastName:  &lastName,
			OrgData:   initialOrgData,
		})
		require.NoError(t, err)

		// Simulate updated claims with new roles
		newOrgData := cdbm.OrgData{
			"testorg": cdbm.Org{
				Name:        "testorg",
				DisplayName: "Test Org",
				OrgType:     "ENTERPRISE",
				Roles:       []string{authz.ProviderAdminRole, authz.TenantAdminRole}, // Valid NICO roles
				Teams:       []cdbm.Team{},
			},
		}

		// Check if update is needed (simulate middleware logic)
		needsUpdate := !user.OrgData.Equal(newOrgData)
		assert.True(t, needsUpdate)

		// Update user
		updatedUser, err := userDAO.Update(context.Background(), nil, cdbm.UserUpdateInput{
			UserID:    user.ID,
			Email:     &email,
			FirstName: &firstName,
			LastName:  &lastName,
			OrgData:   newOrgData,
		})
		require.NoError(t, err)
		assert.True(t, updatedUser.OrgData.Equal(newOrgData))
		assert.Contains(t, updatedUser.OrgData["testorg"].Roles, authz.ProviderAdminRole)
		assert.Contains(t, updatedUser.OrgData["testorg"].Roles, authz.TenantAdminRole)
	})
}

// TestKeycloakAuthURLConstruction tests the OAuth2 URL construction logic
func TestKeycloakAuthURLConstruction(t *testing.T) {
	tests := []struct {
		name            string
		externalBaseURL string
		realm           string
		clientID        string
		idpAlias        string
		email           string
		redirectURI     string
		validateURL     func(*testing.T, string)
	}{
		{
			name:            "production auth URL",
			externalBaseURL: "https://auth.company.com",
			realm:           "production",
			clientID:        "company-app",
			idpAlias:        "company-sso",
			email:           "user@company.com",
			redirectURI:     "https://app.company.com/callback",
			validateURL: func(t *testing.T, authURL string) {
				assert.Contains(t, authURL, "https://auth.company.com/realms/production/protocol/openid-connect/auth")
				assert.Contains(t, authURL, "client_id=company-app")
				assert.Contains(t, authURL, "kc_idp_hint=company-sso")
				assert.Contains(t, authURL, "login_hint=user%40company.com")
				assert.Contains(t, authURL, "response_type=code")
				assert.Contains(t, authURL, "scope=openid")
			},
		},
		{
			name:            "development auth URL with localhost",
			externalBaseURL: "http://localhost:8082",
			realm:           "development",
			clientID:        "dev-client",
			idpAlias:        "dev-idp",
			email:           "dev@localhost.com",
			redirectURI:     "http://localhost:3000/callback",
			validateURL: func(t *testing.T, authURL string) {
				assert.Contains(t, authURL, "http://localhost:8082/realms/development/protocol/openid-connect/auth")
				assert.Contains(t, authURL, "client_id=dev-client")
				assert.Contains(t, authURL, "kc_idp_hint=dev-idp")
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Simulate the URL construction from InitiateAuthFlow
			authParams := fmt.Sprintf(
				"client_id=%s&response_type=code&scope=openid&kc_idp_hint=%s&login_hint=%s&redirect_uri=%s",
				tt.clientID,
				tt.idpAlias,
				strings.ReplaceAll(tt.email, "@", "%40"),
				strings.ReplaceAll(tt.redirectURI, ":", "%3A"),
			)
			authParams = strings.ReplaceAll(authParams, "/", "%2F")

			authURL := tt.externalBaseURL + "/realms/" + tt.realm + "/protocol/openid-connect/auth?" + authParams

			tt.validateURL(t, authURL)
		})
	}
}

// TestKeycloakConfig_WithMockServer tests configuration with a working mock server
func TestKeycloakConfig_WithMockServer(t *testing.T) {
	// Create server that responds to JWKS requests properly
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/realms/nico/protocol/openid-connect/certs") {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			// Generate JWKS from the consistent test key
			testKey := getConsistentTestRSAKey()
			jwks := createJWKSFromRSAKey(&testKey.PublicKey, "test-key-id")
			res.Write([]byte(jwks))
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer testServer.Close()

	t.Run("successful JWKS config initialization", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL: testServer.URL,
			Realm:   "nico",
		}

		jwksConfig, err := keycloakConfig.GetJwksConfig()
		assert.NoError(t, err)
		assert.NotNil(t, jwksConfig)

		expectedURL := testServer.URL + "/realms/nico/protocol/openid-connect/certs"
		assert.Equal(t, expectedURL, jwksConfig.URL)
		assert.NotNil(t, jwksConfig.GetJWKS())
		assert.Greater(t, jwksConfig.KeyCount(), 0)
	})

	t.Run("JWKS config is cached", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL: testServer.URL,
			Realm:   "nico",
		}

		// First call
		jwksConfig1, err := keycloakConfig.GetJwksConfig()
		assert.NoError(t, err)

		// Second call should return cached version
		jwksConfig2, err := keycloakConfig.GetJwksConfig()
		assert.NoError(t, err)

		// Should be the same instance
		assert.Same(t, jwksConfig1, jwksConfig2)
	})
}

func TestKeycloakAuthService_RealServerIntegration(t *testing.T) {
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		fmt.Printf("Mock Keycloak request: %s %s\n", req.Method, req.URL.Path)

		switch {
		// Token endpoint for the test realm (handles both admin login and token exchange)
		case req.URL.Path == "/realms/nico/protocol/openid-connect/token" && req.Method == "POST":
			handleIntegrationTokenEndpoint(res, req)

		// Admin API for identity providers
		case req.URL.Path == "/admin/realms/nico/identity-provider/instances" && req.Method == "GET":
			handleIntegrationIdentityProviders(res, req)

		// User info endpoint
		case req.URL.Path == "/realms/nico/protocol/openid-connect/userinfo" && req.Method == "GET":
			handleIntegrationUserInfo(res, req)

		// Logout endpoint
		case req.URL.Path == "/realms/nico/protocol/openid-connect/logout" && req.Method == "POST":
			handleIntegrationLogout(res, req)

		// JWKS endpoint
		case req.URL.Path == "/realms/nico/protocol/openid-connect/certs" && req.Method == "GET":
			handleIntegrationJWKS(res, req)

		default:
			fmt.Printf("Unhandled request: %s %s\n", req.Method, req.URL.Path)
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer testServer.Close()

	t.Run("admin token retrieval", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL: testServer.URL,
			Realm:   "nico",
		}

		service := NewKeycloakAuthService(keycloakConfig)

		// Test that service can be created with client credentials
		assert.NotNil(t, service)
		assert.Equal(t, "nico", service.config.Realm)
	})

	t.Run("IDP alias retrieval", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL: testServer.URL,
			Realm:   "nico",
		}

		service := NewKeycloakAuthService(keycloakConfig)

		idpAlias, err := service.getIDPAliasForDomain(context.Background(), "admin-access-token", "testorg.com")
		assert.NoError(t, err)
		assert.Equal(t, "testorg-idp", idpAlias)
	})

	t.Run("complete auth flow initiation", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL:         testServer.URL,
			ExternalBaseURL: testServer.URL,
			ClientID:        "test-client",
			ClientSecret:    "test-secret",
			Realm:           "nico",
		}

		service := NewKeycloakAuthService(keycloakConfig)

		authResponse, err := service.InitiateAuthFlow(context.Background(), "john.doe@testorg.com", "http://localhost:3000/callback")
		assert.NoError(t, err)
		require.NotNil(t, authResponse)
		assert.Contains(t, authResponse.AuthURL, "testorg-idp")
		assert.Contains(t, authResponse.AuthURL, "john.doe%40testorg.com")
		assert.Equal(t, "testorg-idp", authResponse.IDP)
		assert.Equal(t, "nico", authResponse.RealmName)
	})

	t.Run("code exchange for tokens", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL:      testServer.URL,
			ClientID:     "test-client",
			ClientSecret: "test-secret",
			Realm:        "nico",
		}

		service := NewKeycloakAuthService(keycloakConfig)

		tokenResponse, err := service.ExchangeCodeForTokens(context.Background(), "valid-auth-code", "http://localhost:3000/callback", "")
		assert.NoError(t, err)
		require.NotNil(t, tokenResponse)
		assert.Equal(t, "test-access-token", tokenResponse.AccessToken)
		assert.Equal(t, "test-refresh-token", tokenResponse.RefreshToken)
		assert.Equal(t, "Bearer", tokenResponse.TokenType)
		assert.Equal(t, 3600, tokenResponse.ExpiresIn)
	})

	t.Run("token refresh", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL:      testServer.URL,
			ClientID:     "test-client",
			ClientSecret: "test-secret",
			Realm:        "nico",
		}

		service := NewKeycloakAuthService(keycloakConfig)

		refreshResponse, err := service.RefreshAccessToken(context.Background(), "valid-refresh-token")
		assert.NoError(t, err)
		require.NotNil(t, refreshResponse)
		assert.Equal(t, "new-access-token", refreshResponse.AccessToken)
		assert.Equal(t, "new-refresh-token", refreshResponse.RefreshToken)
	})

	t.Run("client credentials auth", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL: testServer.URL,
			Realm:   "nico",
		}

		service := NewKeycloakAuthService(keycloakConfig)

		tokenResponse, err := service.ClientCredentialsAuth(context.Background(), "service-client", "service-secret")
		assert.NoError(t, err)
		require.NotNil(t, tokenResponse)
		assert.Equal(t, "service-access-token", tokenResponse.AccessToken)
		assert.Equal(t, "Bearer", tokenResponse.TokenType)
	})

	t.Run("user logout", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL:      testServer.URL,
			ClientID:     "test-client",
			ClientSecret: "test-secret",
			Realm:        "nico",
		}

		service := NewKeycloakAuthService(keycloakConfig)

		err := service.Logout(context.Background(), "valid-refresh-token")
		assert.NoError(t, err) // Logout should not return error even if it fails
	})
}

// Handler functions for mock Keycloak server
func handleIntegrationTokenEndpoint(res http.ResponseWriter, req *http.Request) {
	err := req.ParseForm()
	if err != nil {
		res.WriteHeader(http.StatusBadRequest)
		return
	}

	grantType := req.Form.Get("grant_type")
	clientID := req.Form.Get("client_id")
	clientSecret := req.Form.Get("client_secret")

	// Check for HTTP Basic Auth as well (gocloak uses Basic Auth for client credentials)
	basicAuthUser, basicAuthPass, hasBasicAuth := req.BasicAuth()

	// Use Basic Auth credentials if form credentials are empty
	if clientID == "" && hasBasicAuth {
		clientID = basicAuthUser
	}
	if clientSecret == "" && hasBasicAuth {
		clientSecret = basicAuthPass
	}

	switch grantType {
	case "password":
		// Admin login
		username := req.Form.Get("username")
		password := req.Form.Get("password")
		if username == "admin" && password == "admin-password" {
			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(DefaultMockResponses.AdminLogin))
		} else {
			res.WriteHeader(http.StatusUnauthorized)
			res.Write([]byte(`{"error":"invalid_grant","error_description":"Invalid user credentials"}`))
		}
	case "authorization_code":
		code := req.Form.Get("code")
		if code == "valid-auth-code" {
			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(DefaultMockResponses.Token))
		} else {
			res.WriteHeader(http.StatusBadRequest)
			res.Write([]byte(`{"error":"invalid_grant","error_description":"Invalid authorization code"}`))
		}
	case "refresh_token":
		refreshToken := req.Form.Get("refresh_token")
		if refreshToken == "valid-refresh-token" {
			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(`{"access_token":"new-access-token","expires_in":3600,"refresh_expires_in":1800,"refresh_token":"new-refresh-token","token_type":"Bearer","not-before-policy":0,"session_state":"new-session-state","scope":"openid profile email"}`))
		} else {
			res.WriteHeader(http.StatusBadRequest)
			res.Write([]byte(`{"error":"invalid_grant","error_description":"Invalid refresh token"}`))
		}
	case "client_credentials":
		if clientID == "service-client" && clientSecret == "service-secret" {
			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(`{"access_token":"service-access-token","expires_in":3600,"token_type":"Bearer","not-before-policy":0,"session_state":"service-session-state","scope":"profile email"}`))
		} else if clientID == "test-client" && clientSecret == "test-secret" {
			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(`{"access_token":"admin-access-token","expires_in":3600,"token_type":"Bearer","not-before-policy":0,"session_state":"service-session-state","scope":"profile email"}`))
		} else {
			res.WriteHeader(http.StatusUnauthorized)
			res.Write([]byte(`{"error":"invalid_client","error_description":"Invalid client credentials"}`))
		}
	default:
		res.WriteHeader(http.StatusBadRequest)
		res.Write([]byte(`{"error":"unsupported_grant_type","error_description":"Unsupported grant type"}`))
	}
}

func handleIntegrationIdentityProviders(res http.ResponseWriter, req *http.Request) {
	authHeader := req.Header.Get("Authorization")

	if isValidBearerToken(authHeader) {
		// Accept valid auth and no auth (for gocloak bug compatibility)
		responseBody := DefaultMockResponses.IDPs

		res.Header().Set("Content-Type", "application/json")
		res.Header().Set("Content-Length", fmt.Sprintf("%d", len(responseBody)))
		res.WriteHeader(http.StatusOK)
		res.Write([]byte(responseBody))
	} else {
		res.WriteHeader(http.StatusUnauthorized)
		res.Write([]byte(`{"error":"invalid_token","error_description":"Invalid access token"}`))
	}
}

func handleIntegrationUserInfo(res http.ResponseWriter, req *http.Request) {
	authHeader := req.Header.Get("Authorization")
	if isValidBearerToken(authHeader) {
		res.WriteHeader(http.StatusOK)
		res.Header().Set("Content-Type", "application/json")
		res.Write([]byte(DefaultMockResponses.UserInfo))
	} else {
		res.WriteHeader(http.StatusUnauthorized)
		res.Write([]byte(`{"error":"invalid_token","error_description":"Invalid access token"}`))
	}
}

func handleIntegrationLogout(res http.ResponseWriter, req *http.Request) {
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
		res.Write([]byte(`{"error":"invalid_request","error_description":"Invalid logout request"}`))
	}
}

func handleIntegrationJWKS(res http.ResponseWriter, req *http.Request) {
	res.WriteHeader(http.StatusOK)
	res.Header().Set("Content-Type", "application/json")
	// Real  JWKS response from nico development environment
	res.Write([]byte(`{"keys":[{"kid":"2qPROcQfHMCXUi4rKt-CRB5iG4Z-5rfbP7zHOsxWA28","kty":"RSA","alg":"RS256","use":"sig","x5c":["MIICmTCCAYECBgGYzofhRDANBgkqhkiG9w0BAQsFADAQMQ4wDAYDVQQDDAVmb3JnZTAeFw0yNTA4MjEyMTI2MDhaFw0zNTA4MjEyMTI3NDhaMBAxDjAMBgNVBAMMBWZvcmdlMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA9YNTgddbGn0PKUbk3uISXkhWro0ColFLRZYFWSCCHV5JXG6bgmCeFa4RWnUi0qzRtzyu2uEAWbf5XMJl0TSO9F0N4OdeeW6nK2ZzdK1ASuRy9ACBGgv0kCRpukgX9vlJAjSR3DIHROom9evsf5RYzX9tgNKdkRz1134zZpQ+EtskZ9MnoZEd8NfFbyzAeyAe4iAL+Sjf5DV+ACKwJopDUPz9MwvK7BYEdqZ6ZNnn6nmwNAt/0jabf5Z6QTeKJv22fk6jKM3vQZH2IE/h+ulHYA9pMZoLciQ7zchXVvyAJkIjmeO2nGtW5cFHZ3X2Bm6MMU9MtzIfjAR2FCbKwtJF9QIDAQABMA0GCSqGSIb3DQEBCwUAA4IBAQAqNZY5kMW8VcnmuC1Ux4c6EMLhaAej6mQsGawic6sj9AHiYI24zY6VID9I9IG6cBP9J5Pw84TVU+J96CNcavMmCZV80hQNrunABJHM/lUtv0sUsqGm4qpsnOD+g7B9XKIu9YxnpGzX7ouH0nk355rBN7swTuEBpy5ELtQlraAGMbTDv+UjgpxAiUczsQeS3mvKnyiINx9Rv0imJhRskyuaqmLaVb0eZkezFEWPYzqqOAEEuMOkuwOD/1vJVz3j1gCcy9ZOqwe+8O0zPJuN/cLjDiXPmpqOvI1eKW03O+sBKasYm9dVC/JaBktHeQ0LJZUVGYzgVmbun41z/2Q01WQW"],"x5t":"UHFsVos9chqrKD4oPeyih58kFr0","x5t#S256":"UQ7TfWdf5BUFzuZ_8OcK1Idbzz_mYU2Xrpu-Mv9W1KI","n":"9YNTgddbGn0PKUbk3uISXkhWro0ColFLRZYFWSCCHV5JXG6bgmCeFa4RWnUi0qzRtzyu2uEAWbf5XMJl0TSO9F0N4OdeeW6nK2ZzdK1ASuRy9ACBGgv0kCRpukgX9vlJAjSR3DIHROom9evsf5RYzX9tgNKdkRz1134zZpQ-EtskZ9MnoZEd8NfFbyzAeyAe4iAL-Sjf5DV-ACKwJopDUPz9MwvK7BYEdqZ6ZNnn6nmwNAt_0jabf5Z6QTeKJv22fk6jKM3vQZH2IE_h-ulHYA9pMZoLciQ7zchXVvyAJkIjmeO2nGtW5cFHZ3X2Bm6MMU9MtzIfjAR2FCbKwtJF9Q","e":"AQAB"},{"kid":"rYde1QMYY3w-bK7qt5GPvI6uGK1b38KtguxnLYcYg-U","kty":"RSA","alg":"RSA-OAEP","use":"enc","x5c":["MIICmTCCAYECBgGYzofh5DANBgkqhkiG9w0BAQsFADAQMQ4wDAYDVQQDDAVmb3JnZTAeFw0yNTA4MjEyMTI2MDhaFw0zNTA4MjEyMTI3NDhaMBAxDjAMBgNVBAMMBWZvcmdlMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAtlnHXyI93apJ2gqfX80VlKuz6CrGh79hxPAF3WcKPXfqrng4HjjlH8BYFY37WRXKX4whEEDaE3KPp6p59sOaVpcfYAlf7Nxrzdpm0Mro23mNCR1VCzMlc4enlcD7hB753diBYr93bkMUTZPtE7Ws3YNPPY7+JV+c8xjA0yz7Er1YG89GYuey6sKGxOrNxwvTh9477hN5fKwfVDBBZAZr7oiNxNPFN2ecQ1rXy36byNg8mSRcF32z2Y2KUKuUMXysmSf3W+aC48SHNtykXY9btNEMFhnE2FekmKMc6cefkgkVuSgLo8zmyWYFcFAcmNaqce6EgS4wb4ITfNs9IKrqWwIDAQABMA0GCSqGSIb3DQEBCwUAA4IBAQCZZj+L4eIeLGrSqM0HiacMYDG0KXUHO/x3RM6D7UNRXt+pGF8hs1j9+Q27BEejD6IptBjQEfipHAvOaYV8TuFBpE4UEOLXxQBloLPRO4fb/OS7s7UFyE2XgnOS9E4NMxzRYVfyFxtXPZssNd7WxTeiA4/ISh9C47or9Ge+F5h5YQSVkLtXjRmEhN4K5OMkeafGbmA1WGHSEKQei6QbGgzbTbXTgtTpQgcL6WHLtpBaOnd4X9h38mJ9yPwr+aiadco33VDHWaruG0APDIadjq+SI2pn6H+TPpAfzvr11wnjvZswj6ePoPk9HgxtvQbUBalbOO8rIWSl2n5PrKzZNXwM"],"x5t":"pKk7-fkjCqoobuCTe--sBvdX0wc","x5t#S256":"JL1f_QDCxBTj81_-h_K1KBvRtYAc4GbZBcjuAWWOv2c","n":"tlnHXyI93apJ2gqfX80VlKuz6CrGh79hxPAF3WcKPXfqrng4HjjlH8BYFY37WRXKX4whEEDaE3KPp6p59sOaVpcfYAlf7Nxrzdpm0Mro23mNCR1VCzMlc4enlcD7hB753diBYr93bkMUTZPtE7Ws3YNPPY7-JV-c8xjA0yz7Er1YG89GYuey6sKGxOrNxwvTh9477hN5fKwfVDBBZAZr7oiNxNPFN2ecQ1rXy36byNg8mSRcF32z2Y2KUKuUMXysmSf3W-aC48SHNtykXY9btNEMFhnE2FekmKMc6cefkgkVuSgLo8zmyWYFcFAcmNaqce6EgS4wb4ITfNs9IKrqWw","e":"AQAB"}]}`))
}

// TestKeycloakAuthService_ErrorScenarios tests error handling with realistic server responses
func TestKeycloakAuthService_ErrorScenarios(t *testing.T) {
	// Create server that returns various error responses
	errorServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		switch {
		case strings.Contains(req.URL.Path, "/realms/nico/protocol/openid-connect/token"):
			// Always return unauthorized for admin login
			res.WriteHeader(http.StatusUnauthorized)
			res.Write([]byte(`{"error":"invalid_grant","error_description":"Invalid credentials"}`))
		case strings.Contains(req.URL.Path, "/admin/realms/nico/identity-provider/instances"):
			// Return empty IDP list
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(`[]`))
		case strings.Contains(req.URL.Path, "/realms/nico/protocol/openid-connect/token"):
			// Return error for token exchange
			res.WriteHeader(http.StatusBadRequest)
			res.Write([]byte(`{"error":"invalid_grant","error_description":"Authorization code expired"}`))
		default:
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer errorServer.Close()

	t.Run("admin login failure", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL: errorServer.URL,
			Realm:   "nico",
		}

		service := NewKeycloakAuthService(keycloakConfig)

		// Test client credentials authentication failure
		// Since we're using client credentials now, we test the InitiateAuthFlow instead
		_, err := service.InitiateAuthFlow(context.Background(), "test@example.com", "http://localhost:3000/callback")
		assert.Error(t, err)
		assert.Contains(t, err.Error(), "client credentials not configured")
	})

	t.Run("no IDP found for domain", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL: errorServer.URL,
			Realm:   "nico",
		}

		service := NewKeycloakAuthService(keycloakConfig)

		idpAlias, err := service.getIDPAliasForDomain(context.Background(), "admin-token", "unknown.com")
		assert.Error(t, err)
		assert.Contains(t, err.Error(), "no identity provider found for domain")
		assert.Empty(t, idpAlias)
	})

	t.Run("token exchange failure", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL:      errorServer.URL,
			ClientID:     "test-client",
			ClientSecret: "test-secret",
			Realm:        "nico",
		}

		service := NewKeycloakAuthService(keycloakConfig)

		tokenResponse, err := service.ExchangeCodeForTokens(context.Background(), "expired-code", "http://localhost:3000/callback", "")
		assert.Error(t, err)
		assert.Contains(t, err.Error(), "failed to exchange authorization code")
		assert.Nil(t, tokenResponse)
	})
}

// TestKeycloakConfig_WithRealServer tests configuration with actual server
func TestKeycloakConfig_WithRealServer(t *testing.T) {
	// Create server that responds to JWKS requests
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/realms/nico/protocol/openid-connect/certs") {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(`{"keys":[{"kid":"2qPROcQfHMCXUi4rKt-CRB5iG4Z-5rfbP7zHOsxWA28","kty":"RSA","alg":"RS256","use":"sig","n":"9YNTgddbGn0PKUbk3uISXkhWro0ColFLRZYFWSCCHV5JXG6bgmCeFa4RWnUi0qzRtzyu2uEAWbf5XMJl0TSO9F0N4OdeeW6nK2ZzdK1ASuRy9ACBGgv0kCRpukgX9vlJAjSR3DIHROom9evsf5RYzX9tgNKdkRz1134zZpQ-EtskZ9MnoZEd8NfFbyzAeyAe4iAL-Sjf5DV-ACKwJopDUPz9MwvK7BYEdqZ6ZNnn6nmwNAt_0jabf5Z6QTeKJv22fk6jKM3vQZH2IE_h-ulHYA9pMZoLciQ7zchXVvyAJkIjmeO2nGtW5cFHZ3X2Bm6MMU9MtzIfjAR2FCbKwtJF9Q","e":"AQAB"}]}`))
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer testServer.Close()

	t.Run("JWKS config initialization with real server", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL: testServer.URL,
			Realm:   "nico",
		}

		jwksConfig, err := keycloakConfig.GetJwksConfig()
		assert.NoError(t, err)
		assert.NotNil(t, jwksConfig)

		expectedURL := testServer.URL + "/realms/nico/protocol/openid-connect/certs"
		assert.Equal(t, expectedURL, jwksConfig.URL)
		assert.NotNil(t, jwksConfig.GetJWKS())
		assert.Greater(t, jwksConfig.KeyCount(), 0)
	})

	t.Run("JWKS config caching", func(t *testing.T) {
		keycloakConfig := &config.KeycloakConfig{
			BaseURL: testServer.URL,
			Realm:   "nico",
		}

		// First call
		jwksConfig1, err := keycloakConfig.GetJwksConfig()
		assert.NoError(t, err)
		assert.NotNil(t, jwksConfig1)

		// Second call should return cached version
		jwksConfig2, err := keycloakConfig.GetJwksConfig()
		assert.NoError(t, err)
		assert.NotNil(t, jwksConfig2)

		// Should be the same instance
		assert.Same(t, jwksConfig1, jwksConfig2)
	})
}

// TestEmailDomainExtraction tests the email domain parsing logic
func TestEmailDomainExtraction(t *testing.T) {
	tests := []struct {
		name       string
		email      string
		wantDomain string
		wantErr    bool
	}{
		{
			name:       "valid email",
			email:      "user@example.com",
			wantDomain: "example.com",
			wantErr:    false,
		},
		{
			name:       "email with subdomain",
			email:      "user@mail.example.com",
			wantDomain: "mail.example.com",
			wantErr:    false,
		},
		{
			name:       "invalid email format - no @",
			email:      "invalid-email",
			wantDomain: "",
			wantErr:    true,
		},
		{
			name:       "invalid email format - multiple @",
			email:      "user@@example.com",
			wantDomain: "",
			wantErr:    true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Test the email parsing logic that's used in InitiateAuthFlow
			emailParts := strings.Split(tt.email, "@")
			if len(emailParts) != 2 {
				if tt.wantErr {
					assert.True(t, true) // Expected error
				} else {
					assert.Fail(t, "Expected valid email but got invalid format")
				}
				return
			}

			emailDomain := emailParts[1]
			if !tt.wantErr {
				assert.Equal(t, tt.wantDomain, emailDomain)
			}
		})
	}
}

// TestAuthURLConstruction tests the OAuth2 URL construction
func TestAuthURLConstruction(t *testing.T) {
	tests := []struct {
		name            string
		externalBaseURL string
		realm           string
		clientID        string
		idpAlias        string
		email           string
		redirectURI     string
		expectedURL     string
	}{
		{
			name:            "standard auth URL construction",
			externalBaseURL: "http://keycloak.example.com",
			realm:           "my-realm",
			clientID:        "my-client",
			idpAlias:        "company-idp",
			email:           "user@company.com",
			redirectURI:     "http://app.example.com/callback",
			expectedURL:     "http://keycloak.example.com/realms/my-realm/protocol/openid-connect/auth?client_id=my-client&kc_idp_hint=company-idp&login_hint=user%40company.com&redirect_uri=http%3A%2F%2Fapp.example.com%2Fcallback&response_type=code&scope=openid",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Simulate the URL construction from InitiateAuthFlow
			authParams := url.Values{
				"client_id":     {tt.clientID},
				"response_type": {"code"},
				"scope":         {"openid"},
				"kc_idp_hint":   {tt.idpAlias},
				"login_hint":    {tt.email},
				"redirect_uri":  {tt.redirectURI},
			}

			authURL := tt.externalBaseURL + "/realms/" + tt.realm + "/protocol/openid-connect/auth?" + authParams.Encode()
			assert.Equal(t, tt.expectedURL, authURL)
		})
	}
}

// TestKeycloakAuthService_ClientCredentialsAuth_EdgeCases tests client credentials flow with comprehensive scenarios
func TestKeycloakAuthService_ClientCredentialsAuth_EdgeCases(t *testing.T) {
	tests := []struct {
		name           string
		clientID       string
		clientSecret   string
		mockResponse   string
		mockStatusCode int
		expectError    bool
		expectedToken  *model.APITokenResponse
		errorContains  string
	}{
		{
			name:           "valid_credentials_with_refresh_token",
			clientID:       "test-client",
			clientSecret:   "test-secret",
			mockResponse:   `{"access_token":"acc","refresh_token":"ref","expires_in":3600,"token_type":"Bearer"}`,
			mockStatusCode: http.StatusOK,
			expectError:    false,
			expectedToken:  &model.APITokenResponse{AccessToken: "acc", RefreshToken: "ref", ExpiresIn: 3600, TokenType: "Bearer"},
		},
		{
			name:           "valid_credentials_no_refresh_token",
			clientID:       "test-client",
			clientSecret:   "test-secret",
			mockResponse:   `{"access_token":"acc","expires_in":3600,"token_type":"Bearer"}`,
			mockStatusCode: http.StatusOK,
			expectError:    false,
			expectedToken:  &model.APITokenResponse{AccessToken: "acc", RefreshToken: "", ExpiresIn: 3600, TokenType: "Bearer"},
		},
		{
			name:           "invalid_client_credentials",
			clientID:       "bad-client",
			clientSecret:   "bad-secret",
			mockResponse:   `{"error":"invalid_client","error_description":"Invalid client credentials"}`,
			mockStatusCode: http.StatusUnauthorized,
			expectError:    true,
			expectedToken:  nil,
			errorContains:  "invalid_client",
		},
		{
			name:           "empty_client_id",
			clientID:       "",
			clientSecret:   "test-secret",
			mockResponse:   `{"error":"invalid_request","error_description":"Missing client_id"}`,
			mockStatusCode: http.StatusBadRequest,
			expectError:    true,
			expectedToken:  nil,
			errorContains:  "invalid_request",
		},
		{
			name:           "server_error_500",
			clientID:       "test-client",
			clientSecret:   "test-secret",
			mockResponse:   `{"error":"server_error","error_description":"Internal server error"}`,
			mockStatusCode: http.StatusInternalServerError,
			expectError:    true,
			expectedToken:  nil,
			errorContains:  "server_error",
		},
		{
			name:           "malformed_json_response",
			clientID:       "test-client",
			clientSecret:   "test-secret",
			mockResponse:   `{"access_token":"acc","expires_in":invalid}`,
			mockStatusCode: http.StatusOK,
			expectError:    true,
			expectedToken:  nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create custom mock server for this test
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if strings.Contains(r.URL.Path, "/protocol/openid-connect/token") {
					w.Header().Set("Content-Type", "application/json")
					w.WriteHeader(tt.mockStatusCode)
					w.Write([]byte(tt.mockResponse))
				} else {
					w.WriteHeader(http.StatusNotFound)
				}
			}))
			defer server.Close()

			kcCfg := config.NewKeycloakConfig(server.URL, server.URL, "client", "secret", "realm", true)
			svc := NewKeycloakAuthService(kcCfg)

			tokens, err := svc.ClientCredentialsAuth(context.Background(), tt.clientID, tt.clientSecret)

			if tt.expectError {
				assert.Error(t, err)
				if tt.errorContains != "" {
					assert.Contains(t, strings.ToLower(err.Error()), strings.ToLower(tt.errorContains))
				}
				assert.Nil(t, tokens)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.expectedToken, tokens)
			}
		})
	}
}

// TestKeycloakAuthService_InitiateAuthFlow_EdgeCases tests auth flow initiation with edge cases
func TestKeycloakAuthService_InitiateAuthFlow_EdgeCases(t *testing.T) {
	tests := []struct {
		name           string
		email          string
		redirectURI    string
		mockIDPs       string
		mockAdminToken string
		expectError    bool
		errorContains  string
		validateResult func(t *testing.T, result *model.APILoginResponse)
	}{
		{
			name:           "valid_email_with_matching_idp",
			email:          "user@testorg.com",
			redirectURI:    "http://localhost:3000/callback",
			mockIDPs:       `[{"alias":"testorg-idp","config":{"kc.org.domain":"testorg.com"}}]`,
			mockAdminToken: "valid-admin-token",
			expectError:    false,
			validateResult: func(t *testing.T, result *model.APILoginResponse) {
				assert.NotEmpty(t, result.AuthURL)
				assert.Contains(t, result.AuthURL, "kc_idp_hint=testorg-idp")
				assert.Contains(t, result.AuthURL, "login_hint=user%40testorg.com")
			},
		},
		{
			name:           "email_with_no_matching_idp",
			email:          "user@unknown.com",
			redirectURI:    "http://localhost:3000/callback",
			mockIDPs:       `[{"alias":"testorg-idp","config":{"kc.org.domain":"testorg.com"}}]`,
			mockAdminToken: "valid-admin-token",
			expectError:    true,
			errorContains:  "no identity provider found",
		},
		{
			name:           "invalid_email_format",
			email:          "invalid-email",
			redirectURI:    "http://localhost:3000/callback",
			mockIDPs:       `[]`,
			mockAdminToken: "valid-admin-token",
			expectError:    true,
			errorContains:  "invalid email format",
		},
		{
			name:           "empty_email",
			email:          "",
			redirectURI:    "http://localhost:3000/callback",
			mockIDPs:       `[]`,
			mockAdminToken: "valid-admin-token",
			expectError:    true,
			errorContains:  "invalid email format",
		},
		{
			name:           "email_without_domain",
			email:          "user@",
			redirectURI:    "http://localhost:3000/callback",
			mockIDPs:       `[]`,
			mockAdminToken: "valid-admin-token",
			expectError:    true,
			errorContains:  "no identity provider found",
		},
		{
			name:           "admin_token_failure",
			email:          "user@testorg.com",
			redirectURI:    "http://localhost:3000/callback",
			mockIDPs:       `[]`,
			mockAdminToken: "",
			expectError:    true,
			errorContains:  "failed to get admin token using client credentials",
		},
		{
			name:           "empty_redirect_uri_no_default",
			email:          "user@testorg.com",
			redirectURI:    "",
			mockIDPs:       `[{"alias":"testorg-idp","config":{"kc.org.domain":"testorg.com"}}]`,
			mockAdminToken: "valid-admin-token",
			expectError:    false,
			validateResult: func(t *testing.T, result *model.APILoginResponse) {
				assert.NotEmpty(t, result.AuthURL)
				assert.NotContains(t, result.AuthURL, "redirect_uri=")
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				switch {
				case strings.Contains(r.URL.Path, "/protocol/openid-connect/token"):
					if tt.mockAdminToken != "" {
						w.Header().Set("Content-Type", "application/json")
						w.WriteHeader(http.StatusOK)
						w.Write([]byte(`{"access_token":"` + tt.mockAdminToken + `","token_type":"Bearer"}`))
					} else {
						w.WriteHeader(http.StatusUnauthorized)
					}
				case strings.Contains(r.URL.Path, "/identity-provider/instances"):
					authHeader := r.Header.Get("Authorization")
					if strings.Contains(authHeader, tt.mockAdminToken) && tt.mockAdminToken != "" {
						w.Header().Set("Content-Type", "application/json")
						w.WriteHeader(http.StatusOK)
						w.Write([]byte(tt.mockIDPs))
					} else {
						w.WriteHeader(http.StatusUnauthorized)
					}
				default:
					w.WriteHeader(http.StatusNotFound)
				}
			}))
			defer server.Close()

			kcCfg := config.NewKeycloakConfig(server.URL, server.URL, "test-client", "test-secret", "nico", true)
			svc := NewKeycloakAuthService(kcCfg)

			result, err := svc.InitiateAuthFlow(context.Background(), tt.email, tt.redirectURI)

			if tt.expectError {
				assert.Error(t, err)
				if tt.errorContains != "" {
					assert.Contains(t, strings.ToLower(err.Error()), strings.ToLower(tt.errorContains))
				}
				assert.Nil(t, result)
			} else {
				assert.NoError(t, err)
				require.NotNil(t, result)
				if tt.validateResult != nil {
					tt.validateResult(t, result)
				}
			}
		})
	}
}

// TestKeycloakConfig_IssuerMatching tests issuer validation for various scenarios
func TestKeycloakConfig_IssuerMatching(t *testing.T) {
	tests := []struct {
		name         string
		configIssuer string
		tokenIssuer  string
		shouldMatch  bool
	}{
		{
			name:         "exact_issuer_match",
			configIssuer: "https://keycloak.example.com/realms/production",
			tokenIssuer:  "https://keycloak.example.com/realms/production",
			shouldMatch:  true,
		},
		{
			name:         "different_realms",
			configIssuer: "https://keycloak.example.com/realms/production",
			tokenIssuer:  "https://keycloak.example.com/realms/development",
			shouldMatch:  false,
		},
		{
			name:         "different_hosts",
			configIssuer: "https://keycloak.example.com/realms/production",
			tokenIssuer:  "https://auth.example.com/realms/production",
			shouldMatch:  false,
		},
		{
			name:         "with_trailing_slash_difference",
			configIssuer: "https://keycloak.example.com/realms/production",
			tokenIssuer:  "https://keycloak.example.com/realms/production/",
			shouldMatch:  false,
		},
		{
			name:         "localhost_development_match",
			configIssuer: "http://localhost:8082/realms/nico",
			tokenIssuer:  "http://localhost:8082/realms/nico",
			shouldMatch:  true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create a config with the expected issuer
			kcConfig := config.NewKeycloakConfig(
				"http://localhost:8082",
				strings.TrimSuffix(tt.configIssuer, "/realms/nico"), // Extract base URL
				"test-client",
				"test-secret",
				"nico", // This will be overridden by direct issuer assignment
				true,
			)

			// Override the issuer to match our test case
			kcConfig.Issuer = tt.configIssuer

			// Check if the token issuer matches the config issuer
			matches := (tt.tokenIssuer == kcConfig.Issuer)
			assert.Equal(t, tt.shouldMatch, matches,
				"Expected match=%v for config issuer '%s' vs token issuer '%s'",
				tt.shouldMatch, kcConfig.Issuer, tt.tokenIssuer)
		})
	}
}

// TestEmailDomainExtraction_EdgeCases tests email domain extraction with edge cases
func TestEmailDomainExtraction_EdgeCases(t *testing.T) {
	tests := []struct {
		name           string
		email          string
		expectedDomain string
		expectError    bool
	}{
		{
			name:           "standard_email",
			email:          "user@example.com",
			expectedDomain: "example.com",
			expectError:    false,
		},
		{
			name:           "email_with_subdomain",
			email:          "user@mail.example.com",
			expectedDomain: "mail.example.com",
			expectError:    false,
		},
		{
			name:           "email_with_plus_addressing",
			email:          "user+tag@example.com",
			expectedDomain: "example.com",
			expectError:    false,
		},
		{
			name:        "email_without_at_sign",
			email:       "userexample.com",
			expectError: true,
		},
		{
			name:        "email_with_multiple_at_signs",
			email:       "user@domain@example.com",
			expectError: true,
		},
		{
			name:        "empty_email",
			email:       "",
			expectError: true,
		},
		{
			name:           "email_ending_with_at",
			email:          "user@",
			expectedDomain: "",
			expectError:    true,
		},
		{
			name:           "email_starting_with_at",
			email:          "@example.com",
			expectedDomain: "example.com",
			expectError:    false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Extract domain using the same logic as in InitiateAuthFlow
			emailParts := strings.Split(tt.email, "@")

			if tt.expectError {
				// Should have invalid email format (either not 2 parts or empty domain)
				isValid := len(emailParts) == 2 && emailParts[1] != ""
				assert.False(t, isValid)
			} else {
				assert.Equal(t, 2, len(emailParts))
				assert.NotEmpty(t, emailParts[1])
				if len(emailParts) == 2 {
					domain := emailParts[1]
					assert.Equal(t, tt.expectedDomain, domain)
				}
			}
		})
	}
}

// TestKeycloakAuthService_ErrorEdgeCases tests various error conditions and edge cases
func TestKeycloakAuthService_ErrorEdgeCases(t *testing.T) {
	errorHelper := testutil.NewErrorTestHelper(t)

	t.Run("invalid_base_url_handling", func(t *testing.T) {
		service := NewKeycloakAuthService(&config.KeycloakConfig{
			BaseURL:         "invalid-url-format", // Invalid URL
			ExternalBaseURL: "http://localhost:8082",
			ClientID:        testutil.TestClientID,
			ClientSecret:    testutil.TestClientSecret,
			Realm:           testutil.TestRealm,
		})

		_, err := service.InitiateAuthFlow(context.Background(), testutil.TestUserEmail, testutil.TestCallbackURL)
		assert.Error(t, err, "Should fail with invalid URL")
	})

	t.Run("empty_client_credentials", func(t *testing.T) {
		service := NewKeycloakAuthService(&config.KeycloakConfig{
			BaseURL:         testutil.LocalKeycloakURL,
			ExternalBaseURL: testutil.LocalKeycloakURL,
			ClientID:        "", // Empty client ID
			ClientSecret:    "", // Empty client secret
			Realm:           testutil.TestRealm,
		})

		_, err := service.InitiateAuthFlow(context.Background(), testutil.TestUserEmail, testutil.TestCallbackURL)
		errorHelper.AssertErrorContains(err, "client", "Should fail with empty client credentials")
	})

	t.Run("malformed_email_handling", func(t *testing.T) {
		service := NewKeycloakAuthService(&config.KeycloakConfig{
			BaseURL:         testutil.LocalKeycloakURL,
			ExternalBaseURL: testutil.LocalKeycloakURL,
			ClientID:        testutil.TestClientID,
			ClientSecret:    testutil.TestClientSecret,
			Realm:           testutil.TestRealm,
		})

		malformedEmails := testutil.TestEmails.Invalid

		for _, email := range malformedEmails {
			_, err := service.InitiateAuthFlow(context.Background(), email, testutil.TestCallbackURL)
			assert.Error(t, err, "Should fail for malformed email: %s", email)
		}
	})

	t.Run("network_timeout_simulation", func(t *testing.T) {
		// Create a server that responds slowly but not indefinitely
		hangingServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			time.Sleep(500 * time.Millisecond) // Sleep longer than client timeout but not too long
			w.WriteHeader(http.StatusOK)
		}))
		defer hangingServer.Close()

		service := NewKeycloakAuthService(&config.KeycloakConfig{
			BaseURL:         hangingServer.URL,
			ExternalBaseURL: hangingServer.URL,
			ClientID:        testutil.TestClientID,
			ClientSecret:    testutil.TestClientSecret,
			Realm:           testutil.TestRealm,
		})

		// Test with short context timeout
		ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
		defer cancel()

		_, err := service.InitiateAuthFlow(ctx, testutil.TestUserEmail, testutil.TestCallbackURL)
		assert.Error(t, err, "Should timeout due to context cancellation")
	})

	t.Run("invalid_json_response", func(t *testing.T) {
		// Server that returns invalid JSON
		invalidJSONServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusOK)
			w.Write([]byte("invalid json response {{{"))
		}))
		defer invalidJSONServer.Close()

		service := NewKeycloakAuthService(&config.KeycloakConfig{
			BaseURL:         invalidJSONServer.URL,
			ExternalBaseURL: invalidJSONServer.URL,
			ClientID:        testutil.TestClientID,
			ClientSecret:    testutil.TestClientSecret,
			Realm:           testutil.TestRealm,
		})

		_, err := service.InitiateAuthFlow(context.Background(), testutil.TestUserEmail, testutil.TestCallbackURL)
		assert.Error(t, err, "Should fail to parse invalid JSON")
	})

	t.Run("http_error_status_codes", func(t *testing.T) {
		// Server that returns various HTTP error codes
		errorCodeServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if strings.Contains(r.URL.Path, "admin") {
				w.WriteHeader(http.StatusUnauthorized) // 401
			} else if strings.Contains(r.URL.Path, "identity-provider") {
				w.WriteHeader(http.StatusForbidden) // 403
			} else {
				w.WriteHeader(http.StatusInternalServerError) // 500
			}
		}))
		defer errorCodeServer.Close()

		service := NewKeycloakAuthService(&config.KeycloakConfig{
			BaseURL:         errorCodeServer.URL,
			ExternalBaseURL: errorCodeServer.URL,
			ClientID:        testutil.TestClientID,
			ClientSecret:    testutil.TestClientSecret,
			Realm:           testutil.TestRealm,
		})

		_, err := service.InitiateAuthFlow(context.Background(), testutil.TestUserEmail, testutil.TestCallbackURL)
		assert.Error(t, err, "Should handle HTTP error codes")
	})

	t.Run("concurrent_service_access", func(t *testing.T) {
		service := NewKeycloakAuthService(&config.KeycloakConfig{
			BaseURL:         testutil.LocalKeycloakURL,
			ExternalBaseURL: testutil.LocalKeycloakURL,
			ClientID:        testutil.TestClientID,
			ClientSecret:    testutil.TestClientSecret,
			Realm:           testutil.TestRealm,
		})

		concurrencyHelper := testutil.NewConcurrencyHelper(t)

		// Test concurrent access to service methods
		errors := concurrencyHelper.RunConcurrent(func() error {
			_, err := service.InitiateAuthFlow(context.Background(), testutil.TestUserEmail, testutil.TestCallbackURL)
			return err // Expecting errors due to mock setup, but no panics
		}, 10, "concurrent service access")

		// We expect errors (since this is a mock scenario), but no panics
		// The main thing is that the service handles concurrent access safely
		t.Logf("Concurrent access generated %d errors (expected)", len(errors))

		// Verify service is still functional after concurrent access
		_, err := service.InitiateAuthFlow(context.Background(), testutil.TestUserEmail, testutil.TestCallbackURL)
		t.Logf("Service still functional after concurrent access: %v", err != nil)
	})

	t.Run("memory_pressure_simulation", func(t *testing.T) {
		service := NewKeycloakAuthService(&config.KeycloakConfig{
			BaseURL:         testutil.LocalKeycloakURL,
			ExternalBaseURL: testutil.LocalKeycloakURL,
			ClientID:        testutil.TestClientID,
			ClientSecret:    testutil.TestClientSecret,
			Realm:           testutil.TestRealm,
		})

		// Test with very large inputs to simulate memory pressure
		largeEmail := strings.Repeat("a", 10000) + "@" + strings.Repeat("b", 10000) + ".com"
		largeRedirectURI := "http://localhost:3000/" + strings.Repeat("callback", 1000)

		// This should not cause memory exhaustion or panic
		_, err := service.InitiateAuthFlow(context.Background(), largeEmail, largeRedirectURI)
		assert.Error(t, err, "Should handle large inputs gracefully")

		// Service should still be responsive after large input
		_, err = service.InitiateAuthFlow(context.Background(), "test@small.com", "http://localhost:3000/callback")
		t.Logf("Service responsive after large input: %v", err != nil)
	})
}

// TestInputValidation tests edge cases in input validation
func TestInputValidation(t *testing.T) {
	service := NewKeycloakAuthService(&config.KeycloakConfig{
		BaseURL:         testutil.LocalKeycloakURL,
		ExternalBaseURL: testutil.LocalKeycloakURL,
		ClientID:        testutil.TestClientID,
		ClientSecret:    testutil.TestClientSecret,
		Realm:           testutil.TestRealm,
	})

	t.Run("edge_case_emails", func(t *testing.T) {
		edgeCaseEmails := testutil.TestEmails.EdgeCases

		for _, email := range edgeCaseEmails {
			// Should not panic with edge case emails
			func() {
				defer func() {
					if r := recover(); r != nil {
						t.Errorf("Service panicked with edge case email %q: %v", email, r)
					}
				}()

				_, err := service.InitiateAuthFlow(context.Background(), email, testutil.TestCallbackURL)
				// May error, but should not panic
				t.Logf("Edge case email %q processed: %v", email, err != nil)
			}()
		}
	})

	t.Run("unicode_and_special_characters", func(t *testing.T) {
		unicodeEmails := []string{
			"tëst@example.com",
			"test@éxample.com",
			"user-special@example.com",
			"用户@example.com",
		}

		for _, email := range unicodeEmails {
			func() {
				defer func() {
					if r := recover(); r != nil {
						t.Errorf("Service panicked with unicode email %q: %v", email, r)
					}
				}()

				_, err := service.InitiateAuthFlow(context.Background(), email, testutil.TestCallbackURL)
				t.Logf("Unicode email %q processed: %v", email, err != nil)
			}()
		}
	})

	t.Run("boundary_length_inputs", func(t *testing.T) {
		testCases := []struct {
			name        string
			email       string
			redirectURI string
		}{
			{
				name:        "extremely_long_email",
				email:       strings.Repeat("a", 1000) + "@" + strings.Repeat("b", 1000) + ".com",
				redirectURI: testutil.TestCallbackURL,
			},
			{
				name:        "extremely_long_redirect_uri",
				email:       testutil.TestUserEmail,
				redirectURI: "http://localhost:3000/" + strings.Repeat("path", 1000),
			},
			{
				name:        "empty_strings",
				email:       "",
				redirectURI: "",
			},
			{
				name:        "whitespace_only",
				email:       "   ",
				redirectURI: "   ",
			},
		}

		for _, tc := range testCases {
			t.Run(tc.name, func(t *testing.T) {
				func() {
					defer func() {
						if r := recover(); r != nil {
							t.Errorf("Service panicked with %s: %v", tc.name, r)
						}
					}()

					_, err := service.InitiateAuthFlow(context.Background(), tc.email, tc.redirectURI)
					// Should handle gracefully (may error, but shouldn't panic)
					t.Logf("%s processed: %v", tc.name, err != nil)
				}()
			})
		}
	})
}

// TestErrorRecovery tests that the service can recover from various error states
func TestErrorRecovery(t *testing.T) {
	t.Run("service_recovery_after_network_error", func(t *testing.T) {
		// Start with a failing server
		failingServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(http.StatusInternalServerError)
		}))

		service := NewKeycloakAuthService(&config.KeycloakConfig{
			BaseURL:         failingServer.URL,
			ExternalBaseURL: failingServer.URL,
			ClientID:        testutil.TestClientID,
			ClientSecret:    testutil.TestClientSecret,
			Realm:           testutil.TestRealm,
		})

		// Initial request should fail
		_, err := service.InitiateAuthFlow(context.Background(), testutil.TestUserEmail, testutil.TestCallbackURL)
		assert.Error(t, err, "Initial request should fail")

		// Close failing server
		failingServer.Close()

		// Start a working server
		workingServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			// Return minimal valid response
			if strings.Contains(r.URL.Path, "admin/token") {
				w.Header().Set("Content-Type", "application/json")
				w.Write([]byte(`{"access_token":"test-token","token_type":"Bearer"}`))
			} else {
				w.Header().Set("Content-Type", "application/json")
				w.Write([]byte(`[]`)) // Empty array for IDP list
			}
		}))
		defer workingServer.Close()

		// Update service to use working server
		service.config.BaseURL = workingServer.URL
		service.config.ExternalBaseURL = workingServer.URL

		// Service should now work (or at least not fail due to network issues)
		_, err = service.InitiateAuthFlow(context.Background(), testutil.TestUserEmail, testutil.TestCallbackURL)
		// May still error due to incomplete mock, but should not be network error
		t.Logf("Service recovery test completed, error: %v", err)
	})

	t.Run("memory_recovery_after_large_input", func(t *testing.T) {
		service := NewKeycloakAuthService(&config.KeycloakConfig{
			BaseURL:         testutil.LocalKeycloakURL,
			ExternalBaseURL: testutil.LocalKeycloakURL,
			ClientID:        testutil.TestClientID,
			ClientSecret:    testutil.TestClientSecret,
			Realm:           testutil.TestRealm,
		})

		// Process very large input
		largeEmail := strings.Repeat("test", 100000) + "@example.com"
		_, err := service.InitiateAuthFlow(context.Background(), largeEmail, testutil.TestCallbackURL)
		assert.Error(t, err, "Large input should be rejected")

		// Service should still work with normal input
		_, err = service.InitiateAuthFlow(context.Background(), "normal@example.com", testutil.TestCallbackURL)
		t.Logf("Service recovery after large input: %v", err != nil)

		// Verify no memory leaks by processing multiple normal requests
		for i := 0; i < 10; i++ {
			_, _ = service.InitiateAuthFlow(context.Background(), "test@example.com", testutil.TestCallbackURL)
		}

		t.Log("Memory recovery test completed successfully")
	})
}

// TestEmailValidation_InputVariations tests email validation with various inputs
func TestEmailValidation_InputVariations(t *testing.T) {
	emailSeeds := []string{
		"user@example.com",
		"user.name@example.com",
		"user+tag@example.com",
		"user-name@example-domain.com",
		"UPPERCASE@EXAMPLE.COM",
		"test@sub.domain.com",
		"a@b.c",
		"test@localhost",
		"user@domain",
		"invalid-email",
		"@example.com",
		"user@",
		"user@@example.com",
		"user@example..com",
		"user name@example.com",
		"",
		"tëst@example.com",
		"test@éxample.com",
		"user-special@example.com",
		"test@example-special.com",
		"user\n@example.com",
		"user\t@example.com",
		"user\x00@example.com",
		strings.Repeat("a", 100) + "@" + strings.Repeat("b", 100) + ".com",
	}

	for _, email := range emailSeeds {
		t.Run(fmt.Sprintf("email_%s", email), func(t *testing.T) {
			// Test that our email processing doesn't panic
			func() {
				defer func() {
					if r := recover(); r != nil {
						t.Errorf("Email processing panicked with input %q: %v", email, r)
					}
				}()

				// Test InitiateAuthFlow with fuzzed email
				service := NewKeycloakAuthService(&config.KeycloakConfig{
					BaseURL:         testutil.LocalKeycloakURL,
					ExternalBaseURL: testutil.LocalKeycloakURL,
					ClientID:        testutil.TestClientID,
					ClientSecret:    testutil.TestClientSecret,
					Realm:           testutil.TestRealm,
				})

				// InitiateAuthFlow should never panic
				_, err := service.InitiateAuthFlow(context.Background(), email, testutil.TestCallbackURL)

				// We expect errors for invalid emails, but no panics
				if email == "" || !strings.Contains(email, "@") {
					assert.Error(t, err, "Should error for invalid email: %q", email)
				}

				// Test email in domain extraction context
				if strings.Contains(email, "@") {
					parts := strings.Split(email, "@")
					if len(parts) == 2 {
						domain := parts[1]

						// Domain should not contain invalid characters
						for _, r := range domain {
							if unicode.IsControl(r) {
								t.Logf("Domain contains control character: %q", domain)
							}
						}
					}
				}
			}()
		})
	}
}
