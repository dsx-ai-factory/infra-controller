// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authentication

import (
	"testing"
	"time"

	"github.com/golang-jwt/jwt/v5"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestJWTGenerationHelpers tests the JWT generation helper functions in testing.go
func TestJWTGenerationHelpers(t *testing.T) {
	tests := []struct {
		name        string
		genFunc     func() (string, error)
		expectValid bool
		description string
	}{
		{
			name: "expired_jwt_generation",
			genFunc: func() (string, error) {
				claims := jwt.MapClaims{
					"sub": "test-user",
					"iss": "test-issuer",
					"aud": "test-audience",
				}
				return generateExpiredTestJWT(claims)
			},
			expectValid: false,
			description: "should generate an expired JWT token",
		},
		{
			name: "future_jwt_generation",
			genFunc: func() (string, error) {
				claims := jwt.MapClaims{
					"sub": "test-user",
					"iss": "test-issuer",
					"aud": "test-audience",
				}
				return generateFutureTestJWT(claims)
			},
			expectValid: false,
			description: "should generate a JWT token that's not yet valid",
		},
		{
			name: "wrong_signature_jwt_generation",
			genFunc: func() (string, error) {
				claims := jwt.MapClaims{
					"sub": "test-user",
					"iss": "test-issuer",
					"aud": "test-audience",
					"exp": time.Now().Add(time.Hour).Unix(),
					"iat": time.Now().Unix(),
				}
				return generateJWTWithWrongSignature(claims)
			},
			expectValid: false,
			description: "should generate a JWT token with wrong signature",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tokenString, err := tt.genFunc()
			require.NoError(t, err, "Token generation should not fail")
			assert.NotEmpty(t, tokenString, "Token string should not be empty")

			// Verify token structure by parsing without validation
			token, _, err := new(jwt.Parser).ParseUnverified(tokenString, jwt.MapClaims{})
			require.NoError(t, err, "Token should be parseable even if invalid")

			claims, ok := token.Claims.(jwt.MapClaims)
			require.True(t, ok, "Claims should be MapClaims")

			// Verify basic claims exist
			assert.Contains(t, claims, "sub", "Token should have subject claim")
			assert.Contains(t, claims, "iss", "Token should have issuer claim")

			t.Logf("%s: Generated token with claims: %v", tt.description, claims)
		})
	}
}

func TestMalformedJWTGeneration(t *testing.T) {
	malformedToken := generateMalformedJWT()

	assert.NotEmpty(t, malformedToken, "Malformed token should not be empty")
	assert.Contains(t, malformedToken, "malformed", "Malformed token should contain 'malformed' in payload")

	// Verify it's actually malformed by trying to parse it
	_, _, err := new(jwt.Parser).ParseUnverified(malformedToken, jwt.MapClaims{})
	assert.Error(t, err, "Malformed token should fail to parse")

	t.Logf("Generated malformed JWT: %s", malformedToken)
}

func TestJWTValidationWithGeneratedTokens(t *testing.T) {
	// Test that our generated tokens behave as expected during validation
	testKey := getConsistentTestRSAKey()

	tests := []struct {
		name           string
		tokenGenerator func() (string, error)
		expectError    string
	}{
		{
			name: "expired_token_validation",
			tokenGenerator: func() (string, error) {
				claims := jwt.MapClaims{
					"sub": "test-user",
					"iss": "test-issuer",
				}
				return generateExpiredTestJWT(claims)
			},
			expectError: "token is expired",
		},
		{
			name: "wrong_signature_validation",
			tokenGenerator: func() (string, error) {
				claims := jwt.MapClaims{
					"sub": "test-user",
					"iss": "test-issuer",
					"exp": time.Now().Add(time.Hour).Unix(),
					"iat": time.Now().Unix(),
				}
				return generateJWTWithWrongSignature(claims)
			},
			expectError: "signature is invalid",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tokenString, err := tt.tokenGenerator()
			require.NoError(t, err, "Token generation should succeed")

			// Try to validate the token - this should fail
			parser := jwt.NewParser()
			_, err = parser.Parse(tokenString, func(token *jwt.Token) (interface{}, error) {
				return &testKey.PublicKey, nil
			})

			assert.Error(t, err, "Token validation should fail")
			assert.Contains(t, err.Error(), tt.expectError, "Error should contain expected message")

			t.Logf("Token validation failed as expected: %v", err)
		})
	}
}

func TestMockServerConfigDefaults(t *testing.T) {
	config := DefaultMockServerConfig()

	// Verify default configuration is properly set
	assert.NotNil(t, config.Responses, "Responses should not be nil")
	assert.NotEmpty(t, config.ValidCredentials, "Valid credentials should not be empty")
	assert.NotEmpty(t, config.ValidTokens, "Valid tokens should not be empty")
	assert.NotEmpty(t, config.ValidCodes, "Valid codes should not be empty")

	// Verify specific defaults
	assert.Contains(t, config.ValidCredentials, "admin", "Should contain admin credentials")
	assert.Contains(t, config.ValidTokens, "admin-access-token", "Should contain admin token")
	assert.Contains(t, config.ValidCodes, "valid-auth-code", "Should contain valid auth code")

	// Verify JWKS is properly generated
	assert.NotEmpty(t, config.Responses.JWKS, "JWKS should not be empty")
	assert.Contains(t, config.Responses.JWKS, "keys", "JWKS should contain keys array")

	t.Logf("Default mock server config validated successfully")
}

func TestJWKSGeneration(t *testing.T) {
	testKey := getConsistentTestRSAKey()
	jwks := createJWKSFromRSAKey(&testKey.PublicKey, "test-key-123")

	assert.NotEmpty(t, jwks, "JWKS should not be empty")
	assert.Contains(t, jwks, "test-key-123", "JWKS should contain the specified key ID")
	assert.Contains(t, jwks, "RSA", "JWKS should specify RSA key type")
	assert.Contains(t, jwks, "sig", "JWKS should specify signing use")
	assert.Contains(t, jwks, "RS256", "JWKS should specify RS256 algorithm")

	t.Logf("Generated JWKS: %s", jwks)
}
