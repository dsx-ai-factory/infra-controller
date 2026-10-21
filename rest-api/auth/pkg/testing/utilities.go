// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package testing

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/rsa"
	"encoding/base64"
	"encoding/json"
	"math/big"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/golang-jwt/jwt/v5"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// KeyGenerationHelper provides consistent key generation for tests
type KeyGenerationHelper struct {
	t *testing.T
}

// NewKeyHelper creates a new key generation helper
func NewKeyHelper(t *testing.T) *KeyGenerationHelper {
	return &KeyGenerationHelper{t: t}
}

// GenerateRSAKey generates an RSA key pair for testing
func (h *KeyGenerationHelper) GenerateRSAKey() *rsa.PrivateKey {
	key, err := rsa.GenerateKey(rand.Reader, TestRSAKeySize)
	require.NoError(h.t, err, "Failed to generate RSA key")
	return key
}

// GenerateECDSAKey generates an ECDSA key pair for testing
func (h *KeyGenerationHelper) GenerateECDSAKey() *ecdsa.PrivateKey {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	require.NoError(h.t, err, "Failed to generate ECDSA key")
	return key
}

// GenerateECDSAKeyP384 generates an ECDSA P-384 key pair
func (h *KeyGenerationHelper) GenerateECDSAKeyP384() *ecdsa.PrivateKey {
	key, err := ecdsa.GenerateKey(elliptic.P384(), rand.Reader)
	require.NoError(h.t, err, "Failed to generate ECDSA P-384 key")
	return key
}

// JWTAssertionHelper provides consistent JWT validation patterns
type JWTAssertionHelper struct {
	t *testing.T
}

// NewJWTHelper creates a new JWT assertion helper
func NewJWTHelper(t *testing.T) *JWTAssertionHelper {
	return &JWTAssertionHelper{t: t}
}

// AssertValidJWT validates that a JWT token is properly formed and valid
func (h *JWTAssertionHelper) AssertValidJWT(tokenString string, description string) *jwt.Token {
	token, _ := jwt.Parse(tokenString, func(token *jwt.Token) (interface{}, error) {
		// Return nil to skip signature validation for structure checks
		return nil, nil
	})

	// We expect an error here due to no key provided, but token should be parsed
	assert.NotNil(h.t, token, "JWT should be parseable: %s", description)

	if token != nil {
		assert.NotNil(h.t, token.Claims, "JWT should have claims: %s", description)

		// Validate standard claims if present
		if mapClaims, ok := token.Claims.(jwt.MapClaims); ok {
			h.validateStandardClaims(mapClaims, description)
		}
	}

	return token
}

// AssertJWTHasKid validates that JWT header contains kid
func (h *JWTAssertionHelper) AssertJWTHasKid(tokenString, expectedKid, description string) {
	token, _ := jwt.Parse(tokenString, func(token *jwt.Token) (interface{}, error) {
		return nil, nil // Skip signature validation
	})
	assert.NotNil(h.t, token, "JWT should be parseable")

	if token != nil {
		kid, ok := token.Header["kid"]
		if expectedKid == "" {
			assert.False(h.t, ok, "JWT should not have kid header: %s", description)
		} else {
			assert.True(h.t, ok, "JWT should have kid header: %s", description)
			assert.Equal(h.t, expectedKid, kid, "JWT kid should match expected: %s", description)
		}
	}
}

// AssertJWTAlgorithm validates JWT algorithm
func (h *JWTAssertionHelper) AssertJWTAlgorithm(tokenString, expectedAlg, description string) {
	token, _ := jwt.Parse(tokenString, func(token *jwt.Token) (interface{}, error) {
		return nil, nil
	})
	assert.NotNil(h.t, token, "JWT should be parseable")

	if token != nil {
		alg, ok := token.Header["alg"]
		assert.True(h.t, ok, "JWT should have alg header: %s", description)
		assert.Equal(h.t, expectedAlg, alg, "JWT algorithm should match: %s", description)
	}
}

// validateStandardClaims checks common JWT claims
func (h *JWTAssertionHelper) validateStandardClaims(claims jwt.MapClaims, description string) {
	// Validate issuer if present
	if iss, ok := claims["iss"]; ok {
		assert.NotEmpty(h.t, iss, "Issuer should not be empty: %s", description)
	}

	// Validate subject if present
	if sub, ok := claims["sub"]; ok {
		assert.NotEmpty(h.t, sub, "Subject should not be empty: %s", description)
	}

	// Validate expiration if present
	if exp, ok := claims["exp"]; ok {
		assert.NotNil(h.t, exp, "Expiration should not be nil: %s", description)
	}

	// Validate issued at if present
	if iat, ok := claims["iat"]; ok {
		assert.NotNil(h.t, iat, "Issued at should not be nil: %s", description)
	}
}

// MockServerHelper provides consistent mock server patterns
type MockServerHelper struct {
	t *testing.T
}

// NewMockServerHelper creates a new mock server helper
func NewMockServerHelper(t *testing.T) *MockServerHelper {
	return &MockServerHelper{t: t}
}

// CreateJWKSServer creates a mock JWKS server with given keys
func (h *MockServerHelper) CreateJWKSServer(keys []TestKeyInfo) *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		jwksKeys := make([]map[string]interface{}, 0, len(keys))

		for _, keyInfo := range keys {
			jwkMap := h.createJWKMap(keyInfo.Key, keyInfo.KeyID, keyInfo.Algorithm, keyInfo.Use)
			jwksKeys = append(jwksKeys, jwkMap)
		}

		jwks := map[string]interface{}{
			"keys": jwksKeys,
		}

		jsonData, err := json.Marshal(jwks)
		require.NoError(h.t, err, "Failed to marshal JWKS")

		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		// Mock response writes are best-effort if the client closes the connection.
		_, _ = w.Write(jsonData)
	}))
}

// TestKeyInfo represents key information for testing
type TestKeyInfo struct {
	Key       interface{}
	KeyID     string
	Algorithm string
	Use       string
}

// createJWKMap creates a JWK map representation
func (h *MockServerHelper) createJWKMap(key interface{}, keyID, algorithm, use string) map[string]interface{} {
	jwkMap := map[string]interface{}{
		"kid": keyID,
		"kty": "",
	}

	if algorithm != "" {
		jwkMap["alg"] = algorithm
	}

	if use != "" {
		jwkMap["use"] = use
	}

	switch k := key.(type) {
	case *rsa.PublicKey:
		jwkMap["kty"] = "RSA"
		jwkMap["n"] = EncodeBase64URLBigInt(k.N)
		jwkMap["e"] = EncodeBase64URLBigInt(big.NewInt(int64(k.E)))
	case *ecdsa.PublicKey:
		jwkMap["kty"] = "EC"
		jwkMap["crv"] = getCurveName(k.Curve)
		jwkMap["x"] = EncodeBase64URLBigInt(k.X)
		jwkMap["y"] = EncodeBase64URLBigInt(k.Y)
	}

	return jwkMap
}

// ConcurrencyHelper provides utilities for concurrency testing
type ConcurrencyHelper struct {
	t *testing.T
}

// NewConcurrencyHelper creates a new concurrency test helper
func NewConcurrencyHelper(t *testing.T) *ConcurrencyHelper {
	return &ConcurrencyHelper{t: t}
}

// RunConcurrent executes a function concurrently and collects results
func (h *ConcurrencyHelper) RunConcurrent(fn func() error, numGoroutines int, description string) []error {
	errChan := make(chan error, numGoroutines)

	for range numGoroutines {
		go func() {
			errChan <- fn()
		}()
	}

	var errors []error
	for range numGoroutines {
		if err := <-errChan; err != nil {
			errors = append(errors, err)
		}
	}

	return errors
}

// AssertNoConcurrencyErrors validates that no errors occurred during concurrent execution
func (h *ConcurrencyHelper) AssertNoConcurrencyErrors(errors []error, description string) {
	if len(errors) > 0 {
		h.t.Errorf("Concurrent execution failed: %s", description)
		for i, err := range errors {
			h.t.Errorf("  Error %d: %v", i+1, err)
		}
	}
}

// AssertConcurrentResults validates results from concurrent operations
func (h *ConcurrencyHelper) AssertConcurrentResults(results []interface{}, expectedValue interface{}, description string) {
	for i, result := range results {
		assert.Equal(h.t, expectedValue, result,
			"Concurrent result %d should match expected: %s", i+1, description)
	}
}

// ErrorTestHelper provides utilities for error scenario testing
type ErrorTestHelper struct {
	t *testing.T
}

// NewErrorTestHelper creates a new error test helper
func NewErrorTestHelper(t *testing.T) *ErrorTestHelper {
	return &ErrorTestHelper{t: t}
}

// AssertErrorContains validates that error contains expected message
func (h *ErrorTestHelper) AssertErrorContains(err error, expectedMsg string, description string) {
	assert.Error(h.t, err, "Should have error: %s", description)
	if err != nil {
		assert.Contains(h.t, err.Error(), expectedMsg,
			"Error should contain expected message: %s", description)
	}
}

// AssertErrorType validates error type using interface{}
func (h *ErrorTestHelper) AssertErrorType(err error, expectedType interface{}, description string) {
	assert.Error(h.t, err, "Should have error: %s", description)
	if err != nil {
		assert.IsType(h.t, expectedType, err, "Error should be of expected type: %s", description)
	}
}

// Helper functions (moved from individual test files)

// EncodeBase64URLBigInt encodes a big integer as base64url
func EncodeBase64URLBigInt(i *big.Int) string {
	return base64.RawURLEncoding.EncodeToString(i.Bytes())
}

// getCurveName returns the curve name for ECDSA keys
func getCurveName(curve elliptic.Curve) string {
	switch curve {
	case elliptic.P256():
		return "P-256"
	case elliptic.P384():
		return "P-384"
	case elliptic.P521():
		return "P-521"
	default:
		return "unknown"
	}
}

// TimeHelper provides utilities for time-based testing
type TimeHelper struct {
	t *testing.T
}

// NewTimeHelper creates a new time helper
func NewTimeHelper(t *testing.T) *TimeHelper {
	return &TimeHelper{t: t}
}

// CreateExpiredTime returns a time in the past
func (h *TimeHelper) CreateExpiredTime() time.Time {
	return time.Now().Add(-time.Hour)
}

// CreateFutureTime returns a time in the future
func (h *TimeHelper) CreateFutureTime() time.Time {
	return time.Now().Add(time.Hour)
}

// CreateNearExpiredTime returns a time that's almost expired (within 5 minutes)
func (h *TimeHelper) CreateNearExpiredTime() time.Time {
	return time.Now().Add(4 * time.Minute)
}

// AssertTimeWithinRange validates that a time is within expected range
func (h *TimeHelper) AssertTimeWithinRange(actual, expected time.Time, tolerance time.Duration, description string) {
	diff := actual.Sub(expected)
	if diff < 0 {
		diff = -diff
	}
	assert.LessOrEqual(h.t, diff, tolerance,
		"Time should be within tolerance: %s", description)
}
