// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processors

import (
	"crypto/rand"
	"crypto/rsa"
	"encoding/json"
	"math/big"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/golang-jwt/jwt/v5"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core"
	testutil "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/testing"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
)

// setupTestEnvironment creates a test environment with mock JWKS server and database
func setupTestEnvironment(t *testing.T, audiences []string, scopes []string) (*CustomProcessor, *config.JwksConfig, *rsa.PrivateKey, *httptest.Server, func()) {
	// Generate RSA key for signing tokens
	privateKey, err := rsa.GenerateKey(rand.Reader, 2048)
	require.NoError(t, err)

	// Create JWKS server
	jwksServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		jwks := map[string]interface{}{
			"keys": []map[string]interface{}{
				{
					"kty": "RSA",
					"kid": "test-key-id",
					"use": "sig",
					"alg": "RS256",
					"n":   testutil.EncodeBase64URLBigInt(privateKey.N),
					"e":   testutil.EncodeBase64URLBigInt(big.NewInt(int64(privateKey.E))),
				},
			},
		}
		json.NewEncoder(w).Encode(jwks)
	}))

	// Create JWKS config
	jwksConfig := config.NewJwksConfig(
		"custom-provider",
		jwksServer.URL,
		"https://custom.example.com",
		config.TokenOriginCustom,
		true, // service account enabled
		audiences,
		scopes,
	)

	// Initialize JWKS
	err = jwksConfig.UpdateJWKS()
	require.NoError(t, err)

	// Create test database session
	dbSession := cdbu.GetTestDBSession(t, false)

	// Create processor
	processor := &CustomProcessor{
		dbSession: dbSession,
	}

	cleanup := func() {
		jwksServer.Close()
	}

	return processor, jwksConfig, privateKey, jwksServer, cleanup
}

// createTestToken creates a JWT token with the given claims
func createTestToken(t *testing.T, privateKey *rsa.PrivateKey, claims jwt.MapClaims) string {
	token := jwt.NewWithClaims(jwt.SigningMethodRS256, claims)
	token.Header["kid"] = "test-key-id"

	tokenString, err := token.SignedString(privateKey)
	require.NoError(t, err)
	return tokenString
}

func TestCustomProcessor_ValidateAudiences_Success(t *testing.T) {
	tests := []struct {
		name                string
		configuredAudiences []string
		tokenAudience       interface{} // can be string or []string
		shouldPass          bool
	}{
		{
			name:                "single audience matches",
			configuredAudiences: []string{"api.example.com"},
			tokenAudience:       "api.example.com",
			shouldPass:          true,
		},
		{
			name:                "one of multiple configured audiences matches",
			configuredAudiences: []string{"api.example.com", "app.example.com"},
			tokenAudience:       "app.example.com",
			shouldPass:          true,
		},
		{
			name:                "token has array of audiences, one matches",
			configuredAudiences: []string{"api.example.com"},
			tokenAudience:       []interface{}{"other.example.com", "api.example.com"},
			shouldPass:          true,
		},
		{
			name:                "no configured audiences (validation skipped)",
			configuredAudiences: []string{},
			tokenAudience:       "any.example.com",
			shouldPass:          true,
		},
		{
			name:                "nil configured audiences (validation skipped)",
			configuredAudiences: nil,
			tokenAudience:       "any.example.com",
			shouldPass:          true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			processor, jwksConfig, privateKey, _, cleanup := setupTestEnvironment(t, tt.configuredAudiences, nil)
			defer cleanup()

			claims := jwt.MapClaims{
				"sub": "test-user-123",
				"iss": "https://custom.example.com",
				"aud": tt.tokenAudience,
				"exp": time.Now().Add(time.Hour).Unix(),
				"iat": time.Now().Unix(),
			}

			tokenString := createTestToken(t, privateKey, claims)

			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			logger := zerolog.Nop()

			// Mock the database operations by creating a minimal user
			// In a real test, you'd want to mock the database properly
			_, apiErr := processor.ProcessToken(c, tokenString, jwksConfig, logger)

			if tt.shouldPass {
				// For this test, we're only checking that audience validation passes
				// The actual user creation might fail due to no real DB, but that's OK
				// We just want to ensure no audience-related errors
				if apiErr != nil && apiErr.Code == http.StatusUnauthorized {
					// Check if the error is audience-related
					if contains(apiErr.Message, "audience") {
						t.Errorf("Expected audience validation to pass, but got error: %s", apiErr.Message)
					}
				}
			} else {
				assert.NotNil(t, apiErr, "Expected error for invalid audience")
				if apiErr != nil {
					assert.Equal(t, http.StatusUnauthorized, apiErr.Code)
					assert.Contains(t, apiErr.Message, "audience")
				}
			}
		})
	}
}

func TestCustomProcessor_ValidateAudiences_Failure(t *testing.T) {
	tests := []struct {
		name                string
		configuredAudiences []string
		tokenAudience       interface{}
	}{
		{
			name:                "audience does not match",
			configuredAudiences: []string{"api.example.com"},
			tokenAudience:       "wrong.example.com",
		},
		{
			name:                "token has array but none match",
			configuredAudiences: []string{"api.example.com"},
			tokenAudience:       []interface{}{"other.example.com", "another.example.com"},
		},
		{
			name:                "multiple configured audiences but token has wrong one",
			configuredAudiences: []string{"api.example.com", "app.example.com"},
			tokenAudience:       "wrong.example.com",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			processor, jwksConfig, privateKey, _, cleanup := setupTestEnvironment(t, tt.configuredAudiences, nil)
			defer cleanup()

			claims := jwt.MapClaims{
				"sub": "test-user-123",
				"iss": "https://custom.example.com",
				"aud": tt.tokenAudience,
				"exp": time.Now().Add(time.Hour).Unix(),
				"iat": time.Now().Unix(),
			}

			tokenString := createTestToken(t, privateKey, claims)

			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			logger := zerolog.Nop()

			_, apiErr := processor.ProcessToken(c, tokenString, jwksConfig, logger)

			require.NotNil(t, apiErr, "Expected error for invalid audience")
			assert.Equal(t, http.StatusUnauthorized, apiErr.Code)
			assert.Contains(t, apiErr.Message, "audience")
		})
	}
}

func TestCustomProcessor_ClaimMappingAudiences(t *testing.T) {
	tests := []struct {
		name             string
		mappingAudiences []string
		tokenAudiences   any
		wantCode         int
	}{
		{
			name:             "mapping audience mismatch returns forbidden",
			mappingAudiences: []string{"org-audience"},
			tokenAudiences:   []string{"issuer-audience"},
			wantCode:         http.StatusForbidden,
		},
		{
			name:             "issuer and mapping audiences pass",
			mappingAudiences: []string{"org-audience"},
			tokenAudiences:   []string{"issuer-audience", "org-audience"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			processor, jwksConfig, privateKey, _, cleanup := setupTestEnvironment(t, []string{"issuer-audience"}, []string{"carbide"})
			defer cleanup()

			jwksConfig.ClaimMappings = []config.ClaimMapping{{
				OrgName:   "acme",
				Roles:     []string{"TENANT_ADMIN"},
				Audiences: tt.mappingAudiences,
			}}
			claims := jwt.MapClaims{
				"sub":   "mapping-audience-" + tt.name,
				"iss":   "https://custom.example.com",
				"aud":   tt.tokenAudiences,
				"scope": "carbide",
				"exp":   time.Now().Add(time.Hour).Unix(),
				"iat":   time.Now().Unix(),
			}
			tokenString := createTestToken(t, privateKey, claims)

			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/org/acme/test", nil)
			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)
			c.SetParamNames("orgName")
			c.SetParamValues("acme")

			_, apiErr := processor.ProcessToken(c, tokenString, jwksConfig, zerolog.Nop())
			if tt.wantCode == 0 {
				require.Nil(t, apiErr)
				return
			}

			require.NotNil(t, apiErr)
			assert.Equal(t, tt.wantCode, apiErr.Code)
			assert.Contains(t, apiErr.Message, "not authorized for organization")
		})
	}
}

func TestCustomProcessor_ValidateScopes_Success(t *testing.T) {
	tests := []struct {
		name             string
		configuredScopes []string
		tokenScopes      interface{} // can be string (space-separated) or []string
		shouldPass       bool
	}{
		{
			name:             "single scope matches - nico",
			configuredScopes: []string{"nico"},
			tokenScopes:      "nico",
			shouldPass:       true,
		},
		{
			name:             "multiple scopes all present - includes nico",
			configuredScopes: []string{"nico", "read:data"},
			tokenScopes:      "nico read:data write:data",
			shouldPass:       true,
		},
		{
			name:             "scopes as array",
			configuredScopes: []string{"nico"},
			tokenScopes:      []interface{}{"nico", "other"},
			shouldPass:       true,
		},
		{
			name:             "no configured scopes (validation skipped)",
			configuredScopes: []string{},
			tokenScopes:      "any scope",
			shouldPass:       true,
		},
		{
			name:             "nil configured scopes (validation skipped)",
			configuredScopes: nil,
			tokenScopes:      "any scope",
			shouldPass:       true,
		},
		{
			name:             "all required scopes present in space-separated string",
			configuredScopes: []string{"read:data", "write:data"},
			tokenScopes:      "read:data write:data admin",
			shouldPass:       true,
		},
		{
			name:             "scopes in scp claim instead of scope",
			configuredScopes: []string{"nico"},
			tokenScopes:      "nico", // Will be put in "scp" claim in test
			shouldPass:       true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			processor, jwksConfig, privateKey, _, cleanup := setupTestEnvironment(t, nil, tt.configuredScopes)
			defer cleanup()

			claims := jwt.MapClaims{
				"sub": "test-user-123",
				"iss": "https://custom.example.com",
				"aud": "test-audience",
				"exp": time.Now().Add(time.Hour).Unix(),
				"iat": time.Now().Unix(),
			}

			// Add scope claim
			if tt.name == "scopes in scp claim instead of scope" {
				claims["scp"] = tt.tokenScopes
			} else {
				claims["scope"] = tt.tokenScopes
			}

			tokenString := createTestToken(t, privateKey, claims)

			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			logger := zerolog.Nop()

			_, apiErr := processor.ProcessToken(c, tokenString, jwksConfig, logger)

			if tt.shouldPass {
				// Check that we don't have scope-related errors
				if apiErr != nil && (apiErr.Code == http.StatusUnauthorized || apiErr.Code == http.StatusForbidden) {
					if contains(apiErr.Message, "scope") {
						t.Errorf("Expected scope validation to pass, but got error: %s", apiErr.Message)
					}
				}
			} else {
				assert.NotNil(t, apiErr, "Expected error for invalid scopes")
				if apiErr != nil {
					// Scope validation failures return 403 Forbidden
					assert.Equal(t, http.StatusForbidden, apiErr.Code)
					assert.Contains(t, apiErr.Message, "scope")
				}
			}
		})
	}
}

func TestCustomProcessor_ValidateScopes_Failure(t *testing.T) {
	tests := []struct {
		name             string
		configuredScopes []string
		tokenScopes      interface{}
	}{
		{
			name:             "scope does not match",
			configuredScopes: []string{"nico"},
			tokenScopes:      "other",
		},
		{
			name:             "missing one required scope",
			configuredScopes: []string{"nico", "read:data"},
			tokenScopes:      "nico",
		},
		{
			name:             "completely different scopes",
			configuredScopes: []string{"nico"},
			tokenScopes:      "admin write:data",
		},
		{
			name:             "empty token scopes",
			configuredScopes: []string{"nico"},
			tokenScopes:      "",
		},
		{
			name:             "token has array but missing required scope",
			configuredScopes: []string{"nico", "admin"},
			tokenScopes:      []interface{}{"nico"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			processor, jwksConfig, privateKey, _, cleanup := setupTestEnvironment(t, nil, tt.configuredScopes)
			defer cleanup()

			claims := jwt.MapClaims{
				"sub":   "test-user-123",
				"iss":   "https://custom.example.com",
				"aud":   "test-audience",
				"scope": tt.tokenScopes,
				"exp":   time.Now().Add(time.Hour).Unix(),
				"iat":   time.Now().Unix(),
			}

			tokenString := createTestToken(t, privateKey, claims)

			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			logger := zerolog.Nop()

			_, apiErr := processor.ProcessToken(c, tokenString, jwksConfig, logger)

			// Scope validation failures return 403 Forbidden (not 401 Unauthorized)
			require.NotNil(t, apiErr, "Expected error for invalid scopes")
			assert.Equal(t, http.StatusForbidden, apiErr.Code)
			assert.Contains(t, apiErr.Message, "scope")
		})
	}
}

func TestCustomProcessor_CombinedAudienceAndScope_Validation(t *testing.T) {
	tests := []struct {
		name                string
		configuredAudiences []string
		configuredScopes    []string
		tokenAudience       string
		tokenScopes         string
		shouldPass          bool
		errorShouldContain  string
	}{
		{
			name:                "both audience and scopes valid - nico scope",
			configuredAudiences: []string{"api.example.com"},
			configuredScopes:    []string{"nico"},
			tokenAudience:       "api.example.com",
			tokenScopes:         "nico admin",
			shouldPass:          true,
		},
		{
			name:                "valid audience but invalid scopes",
			configuredAudiences: []string{"api.example.com"},
			configuredScopes:    []string{"nico"},
			tokenAudience:       "api.example.com",
			tokenScopes:         "other",
			shouldPass:          false,
			errorShouldContain:  "scope",
		},
		{
			name:                "invalid audience but valid scopes",
			configuredAudiences: []string{"api.example.com"},
			configuredScopes:    []string{"nico"},
			tokenAudience:       "wrong.example.com",
			tokenScopes:         "nico",
			shouldPass:          false,
			errorShouldContain:  "audience",
		},
		{
			name:                "both invalid",
			configuredAudiences: []string{"api.example.com"},
			configuredScopes:    []string{"nico"},
			tokenAudience:       "wrong.example.com",
			tokenScopes:         "other",
			shouldPass:          false,
			errorShouldContain:  "audience", // Audience is checked first
		},
		{
			name:                "neither configured - both should pass",
			configuredAudiences: nil,
			configuredScopes:    nil,
			tokenAudience:       "any.example.com",
			tokenScopes:         "any",
			shouldPass:          true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			processor, jwksConfig, privateKey, _, cleanup := setupTestEnvironment(t, tt.configuredAudiences, tt.configuredScopes)
			defer cleanup()

			claims := jwt.MapClaims{
				"sub":   "test-user-123",
				"iss":   "https://custom.example.com",
				"aud":   tt.tokenAudience,
				"scope": tt.tokenScopes,
				"exp":   time.Now().Add(time.Hour).Unix(),
				"iat":   time.Now().Unix(),
			}

			tokenString := createTestToken(t, privateKey, claims)

			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)

			logger := zerolog.Nop()

			_, apiErr := processor.ProcessToken(c, tokenString, jwksConfig, logger)

			if tt.shouldPass {
				// May still have DB errors, but no audience/scope errors
				if apiErr != nil && (apiErr.Code == http.StatusUnauthorized || apiErr.Code == http.StatusForbidden) {
					if contains(apiErr.Message, "audience") || contains(apiErr.Message, "scope") {
						t.Errorf("Expected validation to pass, but got error: %s", apiErr.Message)
					}
				}
			} else {
				require.NotNil(t, apiErr, "Expected error for validation failure")
				// Scope failures return 403, audience failures return 401
				if tt.errorShouldContain == "scope" {
					assert.Equal(t, http.StatusForbidden, apiErr.Code)
				} else {
					assert.Equal(t, http.StatusUnauthorized, apiErr.Code)
				}
				assert.Contains(t, apiErr.Message, tt.errorShouldContain)
			}
		})
	}
}

func TestCustomProcessor_MissingAudienceClaim(t *testing.T) {
	processor, jwksConfig, privateKey, _, cleanup := setupTestEnvironment(t, []string{"api.example.com"}, nil)
	defer cleanup()

	claims := jwt.MapClaims{
		"sub": "test-user-123",
		"iss": "https://custom.example.com",
		// No "aud" claim
		"exp": time.Now().Add(time.Hour).Unix(),
		"iat": time.Now().Unix(),
	}

	tokenString := createTestToken(t, privateKey, claims)

	e := echo.New()
	req := httptest.NewRequest(http.MethodGet, "/", nil)
	rec := httptest.NewRecorder()
	c := e.NewContext(req, rec)

	logger := zerolog.Nop()

	_, apiErr := processor.ProcessToken(c, tokenString, jwksConfig, logger)

	require.NotNil(t, apiErr, "Expected error for missing audience claim")
	assert.Equal(t, http.StatusUnauthorized, apiErr.Code)
	assert.Contains(t, apiErr.Message, "audience")
}

func TestCustomProcessor_MissingScopeClaim(t *testing.T) {
	processor, jwksConfig, privateKey, _, cleanup := setupTestEnvironment(t, nil, []string{"nico"})
	defer cleanup()

	claims := jwt.MapClaims{
		"sub": "test-user-123",
		"iss": "https://custom.example.com",
		"aud": "test-audience",
		// No "scope" or "scp" claim
		"exp": time.Now().Add(time.Hour).Unix(),
		"iat": time.Now().Unix(),
	}

	tokenString := createTestToken(t, privateKey, claims)

	e := echo.New()
	req := httptest.NewRequest(http.MethodGet, "/", nil)
	rec := httptest.NewRecorder()
	c := e.NewContext(req, rec)

	logger := zerolog.Nop()

	_, apiErr := processor.ProcessToken(c, tokenString, jwksConfig, logger)

	// Missing scope claim when scopes are required returns 403 Forbidden
	require.NotNil(t, apiErr, "Expected error for missing scope claim")
	assert.Equal(t, http.StatusForbidden, apiErr.Code)
	assert.Contains(t, apiErr.Message, "scope")
}

// Direct validation tests - these test the validation functions without database dependencies

func TestValidateAudiences_DirectTest(t *testing.T) {
	tests := []struct {
		name                string
		tokenClaims         jwt.MapClaims
		configuredAudiences []string
		shouldPass          bool
	}{
		{
			name: "single audience matches - nico example",
			tokenClaims: jwt.MapClaims{
				"aud": "api.nico.com",
			},
			configuredAudiences: []string{"api.nico.com"},
			shouldPass:          true,
		},
		{
			name: "multiple audiences, one matches",
			tokenClaims: jwt.MapClaims{
				"aud": []interface{}{"other.com", "api.nico.com"},
			},
			configuredAudiences: []string{"api.nico.com"},
			shouldPass:          true,
		},
		{
			name: "audience mismatch",
			tokenClaims: jwt.MapClaims{
				"aud": "wrong.com",
			},
			configuredAudiences: []string{"api.nico.com"},
			shouldPass:          false,
		},
		{
			name: "missing audience claim",
			tokenClaims: jwt.MapClaims{
				"sub": "test",
			},
			configuredAudiences: []string{"api.nico.com"},
			shouldPass:          false,
		},
		{
			name: "no configured audiences - validation skipped",
			tokenClaims: jwt.MapClaims{
				"aud": "any.com",
			},
			configuredAudiences: []string{},
			shouldPass:          true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			jwksConfig := &config.JwksConfig{
				Name:      "test",
				Audiences: tt.configuredAudiences,
			}
			err := jwksConfig.ValidateAudience(tt.tokenClaims)

			if tt.shouldPass {
				assert.NoError(t, err, "Expected no error for valid audience")
			} else {
				assert.ErrorIs(t, err, core.ErrInvalidAudience, "Expected invalid audience error")
			}
		})
	}
}

func TestValidateScopes_DirectTest(t *testing.T) {
	tests := []struct {
		name           string
		tokenClaims    jwt.MapClaims
		requiredScopes []string
		shouldPass     bool
	}{
		{
			name: "single scope matches - scopes array with nico",
			tokenClaims: jwt.MapClaims{
				"scopes": []interface{}{"nico"},
			},
			requiredScopes: []string{"nico"},
			shouldPass:     true,
		},
		{
			name: "multiple scopes in token, all required present - nico included",
			tokenClaims: jwt.MapClaims{
				"scopes": []interface{}{"nico", "read:data", "write:data"},
			},
			requiredScopes: []string{"nico", "read:data"},
			shouldPass:     true,
		},
		{
			name: "scopes as space-separated string",
			tokenClaims: jwt.MapClaims{
				"scopes": "nico admin read:data",
			},
			requiredScopes: []string{"nico"},
			shouldPass:     true,
		},
		{
			name: "missing required scope",
			tokenClaims: jwt.MapClaims{
				"scopes": []interface{}{"read:data"},
			},
			requiredScopes: []string{"nico"},
			shouldPass:     false,
		},
		{
			name: "missing scope claim",
			tokenClaims: jwt.MapClaims{
				"sub": "test",
			},
			requiredScopes: []string{"nico"},
			shouldPass:     false,
		},
		{
			name: "no required scopes - validation skipped",
			tokenClaims: jwt.MapClaims{
				"scopes": []interface{}{"any"},
			},
			requiredScopes: []string{},
			shouldPass:     true,
		},
		{
			name: "scp claim as fallback - space-separated",
			tokenClaims: jwt.MapClaims{
				"scp": "nico admin",
			},
			requiredScopes: []string{"nico"},
			shouldPass:     true,
		},
		{
			name: "scope in scopes claim (plural) - kas token format",
			tokenClaims: jwt.MapClaims{
				"scopes": []interface{}{"kas"},
			},
			requiredScopes: []string{"kas"},
			shouldPass:     true,
		},
		{
			name: "multiple required scopes, all present",
			tokenClaims: jwt.MapClaims{
				"scopes": []interface{}{"nico", "read:data", "write:data", "admin"},
			},
			requiredScopes: []string{"nico", "read:data", "admin"},
			shouldPass:     true,
		},
		{
			name: "multiple required scopes, one missing",
			tokenClaims: jwt.MapClaims{
				"scopes": []interface{}{"nico", "read:data"},
			},
			requiredScopes: []string{"nico", "admin"},
			shouldPass:     false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			jwksConfig := &config.JwksConfig{
				Name:   "test",
				Scopes: tt.requiredScopes,
			}
			err := jwksConfig.ValidateScopes(tt.tokenClaims)

			if tt.shouldPass {
				assert.NoError(t, err, "Expected no error for valid scopes")
			} else {
				assert.ErrorIs(t, err, core.ErrInvalidScope, "Expected invalid scope error")
			}
		})
	}
}

func TestCombinedValidation_DirectTest(t *testing.T) {
	tests := []struct {
		name                string
		tokenClaims         jwt.MapClaims
		configuredAudiences []string
		requiredScopes      []string
		shouldPass          bool
		expectAudienceErr   bool
		expectScopeErr      bool
	}{
		{
			name: "both audience and scopes valid - nico example",
			tokenClaims: jwt.MapClaims{
				"aud":    "api.nico.com",
				"scopes": []interface{}{"nico", "read:data"},
			},
			configuredAudiences: []string{"api.nico.com"},
			requiredScopes:      []string{"nico"},
			shouldPass:          true,
		},
		{
			name: "valid audience, invalid scopes",
			tokenClaims: jwt.MapClaims{
				"aud":    "api.nico.com",
				"scopes": []interface{}{"other"},
			},
			configuredAudiences: []string{"api.nico.com"},
			requiredScopes:      []string{"nico"},
			shouldPass:          false,
			expectScopeErr:      true,
		},
		{
			name: "invalid audience, valid scopes",
			tokenClaims: jwt.MapClaims{
				"aud":    "wrong.com",
				"scopes": []interface{}{"nico"},
			},
			configuredAudiences: []string{"api.nico.com"},
			requiredScopes:      []string{"nico"},
			shouldPass:          false,
			expectAudienceErr:   true,
		},
		{
			name: "both invalid",
			tokenClaims: jwt.MapClaims{
				"aud":    "wrong.com",
				"scopes": []interface{}{"other"},
			},
			configuredAudiences: []string{"api.nico.com"},
			requiredScopes:      []string{"nico"},
			shouldPass:          false,
			expectAudienceErr:   true, // Audience checked first
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			jwksConfig := &config.JwksConfig{
				Name:      "test",
				Audiences: tt.configuredAudiences,
				Scopes:    tt.requiredScopes,
			}

			// Test audience first (mirrors actual validation order)
			audErr := jwksConfig.ValidateAudience(tt.tokenClaims)

			// Only test scopes if audience passed
			var scopeErr error
			if audErr == nil {
				scopeErr = jwksConfig.ValidateScopes(tt.tokenClaims)
			}

			if tt.shouldPass {
				assert.NoError(t, audErr, "Expected no audience error")
				assert.NoError(t, scopeErr, "Expected no scope error")
			} else {
				if tt.expectAudienceErr {
					assert.ErrorIs(t, audErr, core.ErrInvalidAudience, "Expected invalid audience error")
				}
				if tt.expectScopeErr {
					assert.ErrorIs(t, scopeErr, core.ErrInvalidScope, "Expected invalid scope error")
				}
			}
		})
	}
}

// Helper function to check if a string contains a substring (case-insensitive)
func contains(s, substr string) bool {
	return len(s) >= len(substr) && (s == substr || len(substr) == 0 ||
		(len(s) > 0 && len(substr) > 0 && containsHelper(s, substr)))
}

func containsHelper(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}
