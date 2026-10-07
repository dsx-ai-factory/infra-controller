// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/rsa"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core"
	mapset "github.com/deckarep/golang-set/v2"
	"github.com/go-jose/go-jose/v4"
	"github.com/golang-jwt/jwt/v5"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestNewJwksConfig tests the constructor with various inputs including edge cases
func TestNewJwksConfig(t *testing.T) {
	tests := []struct {
		name     string
		url      string
		issuer   string
		validate func(t *testing.T, config *JwksConfig)
	}{
		{
			name:   "initialize_RSA_jwks_config",
			url:    "https://rsa.test.com/.well-known/jwks.json",
			issuer: "test.issuer.com",
			validate: func(t *testing.T, config *JwksConfig) {
				assert.Equal(t, "https://rsa.test.com/.well-known/jwks.json", config.URL)
				assert.Equal(t, "test.issuer.com", config.Issuer)
			},
		},
		{
			name:   "initialize_ECDSA_jwks_config",
			url:    "https://ecdsa.test.com/.well-known/jwks.json",
			issuer: "test.issuer.com",
			validate: func(t *testing.T, config *JwksConfig) {
				assert.Equal(t, "https://ecdsa.test.com/.well-known/jwks.json", config.URL)
				assert.Equal(t, "test.issuer.com", config.Issuer)
			},
		},
		{
			name:   "empty_url",
			url:    "",
			issuer: "test.example.com",
			validate: func(t *testing.T, config *JwksConfig) {
				assert.Equal(t, "", config.URL)
				assert.Equal(t, "test.example.com", config.Issuer)
			},
		},
		{
			name:   "empty_issuer",
			url:    "https://example.com/.well-known/jwks.json",
			issuer: "",
			validate: func(t *testing.T, config *JwksConfig) {
				assert.Equal(t, "https://example.com/.well-known/jwks.json", config.URL)
				assert.Equal(t, "", config.Issuer)
			},
		},
		{
			name:   "special_characters_in_issuer",
			url:    "https://example.com/.well-known/jwks.json",
			issuer: "test-env.example.com/realms/test-realm",
			validate: func(t *testing.T, config *JwksConfig) {
				assert.Equal(t, "https://example.com/.well-known/jwks.json", config.URL)
				assert.Equal(t, "test-env.example.com/realms/test-realm", config.Issuer)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			config := NewJwksConfig("test-config", tt.url, tt.issuer, TokenOriginKasSsa, false, nil, nil)
			require.NotNil(t, config)
			tt.validate(t, config)

			// Verify initial state
			assert.True(t, config.LastUpdated.IsZero())
			assert.Nil(t, config.jwks)
		})
	}
}

// TestJwksConfig_MatchesIssuer tests exact issuer matching with various edge cases
func TestJwksConfig_MatchesIssuer(t *testing.T) {
	tests := []struct {
		name          string
		configIssuer  string
		testIssuer    string
		expectedMatch bool
		description   string
	}{
		{
			name:          "empty_config_issuer_matches_none",
			configIssuer:  "",
			testIssuer:    "https://keycloak.example.com/realms/test",
			expectedMatch: false,
			description:   "Empty issuer should match no issuers",
		},
		{
			name:          "exact_match",
			configIssuer:  "https://keycloak.example.com/realms/test",
			testIssuer:    "https://keycloak.example.com/realms/test",
			expectedMatch: true,
			description:   "Should match when issuers are exactly the same",
		},
		{
			name:          "partial_match_fails",
			configIssuer:  "keycloak.example.com",
			testIssuer:    "https://keycloak.example.com/realms/test",
			expectedMatch: false,
			description:   "Should not match when only part of issuer matches",
		},
		{
			name:          "subdomain_no_match",
			configIssuer:  "keycloak.example.com",
			testIssuer:    "https://auth.keycloak.example.com/realms/test",
			expectedMatch: false,
			description:   "Should not match subdomain with different issuer string",
		},
		{
			name:          "case_sensitive_match",
			configIssuer:  "HTTPS://EXAMPLE.COM",
			testIssuer:    "https://example.com",
			expectedMatch: false,
			description:   "Exact matching should be case sensitive",
		},
		{
			name:          "ssa_exact_match",
			configIssuer:  "ytynxseffxl4u4jswpl8k6wfcbjzudh1k9dmmlfnquw.stg.ssa.nvidia.com",
			testIssuer:    "ytynxseffxl4u4jswpl8k6wfcbjzudh1k9dmmlfnquw.stg.ssa.nvidia.com",
			expectedMatch: true,
			description:   "Should match exact SSA issuer",
		},
		{
			name:          "kas_exact_match",
			configIssuer:  "authn.nvidia.com",
			testIssuer:    "authn.nvidia.com",
			expectedMatch: true,
			description:   "Should match exact KAS issuer",
		},
		{
			name:          "empty_test_issuer",
			configIssuer:  "example.com",
			testIssuer:    "",
			expectedMatch: false,
			description:   "Empty test issuer should not match any issuer",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			config := &JwksConfig{
				URL:    "https://example.com/.well-known/jwks.json",
				Issuer: tt.configIssuer,
			}

			result := config.MatchesIssuer(tt.testIssuer)
			assert.Equal(t, tt.expectedMatch, result, tt.description)
		})
	}
}

// TestJwksConfig_UpdateJWKs tests JWKS updates with various error scenarios
func TestJwksConfig_UpdateJWKs(t *testing.T) {
	tests := []struct {
		name           string
		serverResponse func(w http.ResponseWriter, r *http.Request)
		expectError    bool
		errorContains  string
	}{
		{
			name: "successful_update",
			serverResponse: func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(http.StatusOK)
				// Return a valid JWKS with at least one key
				w.Write([]byte(`{"keys": [{"kty":"RSA","use":"sig","kid":"test-key-1","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`))
			},
			expectError: false,
		},
		{
			name: "invalid_json_response",
			serverResponse: func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(http.StatusOK)
				w.Write([]byte(`{"keys": [invalid json`))
			},
			expectError:   true,
			errorContains: "invalid",
		},
		{
			name: "server_error_500",
			serverResponse: func(w http.ResponseWriter, r *http.Request) {
				w.WriteHeader(http.StatusInternalServerError)
			},
			expectError:   true,
			errorContains: "500",
		},
		{
			name: "server_error_404",
			serverResponse: func(w http.ResponseWriter, r *http.Request) {
				w.WriteHeader(http.StatusNotFound)
			},
			expectError:   true,
			errorContains: "404",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create test server with custom response
			server := httptest.NewServer(http.HandlerFunc(tt.serverResponse))
			defer server.Close()

			config := &JwksConfig{
				URL:    server.URL,
				Issuer: "test.example.com",
			}

			err := config.UpdateJWKS()

			if tt.expectError {
				assert.Error(t, err)
				if tt.errorContains != "" {
					assert.Contains(t, strings.ToLower(err.Error()), strings.ToLower(tt.errorContains))
				}
			} else {
				assert.NoError(t, err)
			}
		})
	}

	t.Run("failed update is throttled while cache is empty", func(t *testing.T) {
		requestCount := 0
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			requestCount++
			w.WriteHeader(http.StatusServiceUnavailable)
		}))
		defer server.Close()

		config := &JwksConfig{
			URL:    server.URL,
			Issuer: "test.example.com",
		}

		require.Error(t, config.UpdateJWKS())
		assert.False(t, config.LastAttempted.IsZero())
		assert.True(t, config.LastUpdated.IsZero())

		assert.ErrorIs(t, config.UpdateJWKS(), core.ErrJWKSNotInitialized)
		assert.Equal(t, 1, requestCount)

		config.Lock()
		config.LastAttempted = time.Now().Add(-minUpdateInterval)
		config.Unlock()

		require.Error(t, config.UpdateJWKS())
		assert.Equal(t, 2, requestCount)
	})

	t.Run("in-flight update remains retryable", func(t *testing.T) {
		tests := []struct {
			name      string
			warmCache bool
		}{
			{name: "cold cache"},
			{name: "warm cache", warmCache: true},
		}

		for _, tt := range tests {
			t.Run(tt.name, func(t *testing.T) {
				privateKey, err := rsa.GenerateKey(rand.Reader, 2048)
				require.NoError(t, err)

				requestStarted := make(chan struct{})
				releaseResponse := make(chan struct{})
				responseResult := make(chan error, 2)
				var requestStartedOnce sync.Once
				var releaseResponseOnce sync.Once
				release := func() {
					releaseResponseOnce.Do(func() { close(releaseResponse) })
				}

				server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
					requestStartedOnce.Do(func() { close(requestStarted) })
					<-releaseResponse
					w.Header().Set("Content-Type", "application/json")
					w.WriteHeader(http.StatusOK)
					_, writeErr := w.Write([]byte(createJWKSResponse(privateKey.Public().(*rsa.PublicKey), "new-key-id", "RS256", "sig")))
					responseResult <- writeErr
				}))
				defer server.Close()
				defer release()

				config := &JwksConfig{
					URL:    server.URL,
					Issuer: "test.example.com",
				}
				if tt.warmCache {
					oldPrivateKey, keyErr := rsa.GenerateKey(rand.Reader, 2048)
					require.NoError(t, keyErr)
					config.jwks = &core.JWKS{Set: &jose.JSONWebKeySet{Keys: []jose.JSONWebKey{{
						Key:       oldPrivateKey.Public(),
						KeyID:     "old-key-id",
						Algorithm: "RS256",
						Use:       "sig",
					}}}}
				}

				tokenString, err := createTokenWithGoJose(privateKey, true, "new-key-id")
				require.NoError(t, err)
				updateResult := make(chan error, 1)
				go func() {
					updateResult <- config.UpdateJWKS()
				}()

				select {
				case <-requestStarted:
				case <-time.After(2 * time.Second):
					t.Fatal("timed out waiting for JWKS request to start")
				}

				type validationResult struct {
					token *jwt.Token
					err   error
				}
				validationDone := make(chan validationResult, 1)
				go func() {
					token, validationErr := config.ValidateToken(tokenString, jwt.MapClaims{})
					validationDone <- validationResult{token: token, err: validationErr}
				}()

				select {
				case result := <-validationDone:
					t.Fatalf("validation returned before the in-flight update completed: %v", result.err)
				case <-time.After(50 * time.Millisecond):
				}

				release()
				select {
				case err := <-responseResult:
					require.NoError(t, err)
				case <-time.After(2 * time.Second):
					t.Fatal("timed out waiting for JWKS response")
				}
				select {
				case err := <-updateResult:
					require.NoError(t, err)
				case <-time.After(2 * time.Second):
					t.Fatal("timed out waiting for JWKS update")
				}
				select {
				case result := <-validationDone:
					require.NoError(t, result.err)
					require.NotNil(t, result.token)
					assert.True(t, result.token.Valid)
				case <-time.After(2 * time.Second):
					t.Fatal("timed out waiting for token validation")
				}
				select {
				case err := <-responseResult:
					t.Fatalf("token validation started an unexpected second JWKS request: %v", err)
				default:
				}
			})
		}
	})
}

// TestJwksConfig_Concurrency tests thread safety of JWKS operations
func TestJwksConfig_Concurrency(t *testing.T) {
	// Create a test server that returns valid JWKS
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(`{
			"keys": [
				{
					"kty": "RSA",
					"use": "sig",
					"kid": "test-key-1",
					"alg": "RS256",
					"n": "test-modulus",
					"e": "AQAB"
				}
			]
		}`))
	}))
	defer server.Close()

	config := &JwksConfig{
		URL:    server.URL,
		Issuer: "test.example.com",
	}

	// Test concurrent access to UpdateAllJWKS
	const numGoroutines = 10
	var wg sync.WaitGroup
	errors := make(chan error, numGoroutines)

	for i := 0; i < numGoroutines; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			err := config.UpdateJWKS()
			if err != nil {
				errors <- err
			}
		}()
	}

	wg.Wait()
	close(errors)

	// Check for errors - allow "update already in progress" as this is expected concurrent behavior
	var unexpectedErrors []error
	var updateInProgressCount int
	for err := range errors {
		if err == core.ErrJWKSUpdateInProgress {
			updateInProgressCount++
		} else {
			unexpectedErrors = append(unexpectedErrors, err)
		}
	}

	// Should have at most 9 "update in progress" errors (since 10 goroutines, 1 succeeds)
	if updateInProgressCount > numGoroutines-1 {
		t.Errorf("Too many 'update in progress' errors: expected at most %d, got %d", numGoroutines-1, updateInProgressCount)
	}

	// Should not have any other types of errors
	for _, err := range unexpectedErrors {
		t.Errorf("Concurrent UpdateAllJWKS failed with unexpected error: %v", err)
	}

	// Test concurrent read operations
	wg = sync.WaitGroup{}
	for i := 0; i < numGoroutines; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			// These should not panic or cause race conditions
			_ = config.KeyCount()
			_ = config.MatchesIssuer("https://test.example.com/realms/test")
			_, _ = config.GetKeyByID("test-key-1")
		}()
	}

	wg.Wait()
}

// TestJwksConfig_KeyOperations tests key-related operations with edge cases
func TestJwksConfig_KeyOperations(t *testing.T) {
	t.Run("operations_on_uninitialized_jwks", func(t *testing.T) {
		config := &JwksConfig{
			URL:    "https://example.com/.well-known/jwks.json",
			Issuer: "test.example.com",
		}

		// Test operations on uninitialized JWKS
		assert.Equal(t, 0, config.KeyCount())
		assert.Nil(t, config.GetJWKS())

		key, err := config.GetKeyByID("any-key")
		assert.Nil(t, key)
		assert.Contains(t, err.Error(), "JWKS not initialized") // Updated error message

		// Key lookup should fail for uninitialized JWKS
		_, keyErr := config.GetKeyByID("any-key")
		assert.Error(t, keyErr, "Should error for uninitialized JWKS")
	})

	t.Run("operations_after_failed_update", func(t *testing.T) {
		// Create server that returns error
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(http.StatusInternalServerError)
		}))
		defer server.Close()

		config := &JwksConfig{
			URL:    server.URL,
			Issuer: "test.example.com",
		}

		// Update should fail
		err := config.UpdateJWKS()
		assert.Error(t, err)

		// Operations should still work safely
		assert.Equal(t, 0, config.KeyCount())
	})
}

// TestJwksConfig_LastUpdate tests the LastUpdated timestamp functionality
func TestJwksConfig_LastUpdate(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		// Return a valid JWKS with at least one key
		w.Write([]byte(`{"keys": [{"kty":"RSA","use":"sig","kid":"test-key-1","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`))
	}))
	defer server.Close()

	config := &JwksConfig{
		URL:    server.URL,
		Issuer: "test.example.com",
	}

	// Initially, LastUpdated should be zero
	assert.True(t, config.LastUpdated.IsZero())

	// After successful update, LastUpdated should be set
	beforeUpdate := time.Now()
	err := config.UpdateJWKS()
	afterUpdate := time.Now()

	require.NoError(t, err)
	assert.False(t, config.LastUpdated.IsZero())
	assert.True(t, config.LastUpdated.After(beforeUpdate) || config.LastUpdated.Equal(beforeUpdate))
	assert.True(t, config.LastUpdated.Before(afterUpdate) || config.LastUpdated.Equal(afterUpdate))

	// Second update within 10 seconds should be throttled and not update timestamp
	firstUpdate := config.LastUpdated
	time.Sleep(10 * time.Millisecond) // Small delay - still within throttle window

	err = config.UpdateJWKS()
	require.NoError(t, err) // Should succeed but be throttled
	assert.Equal(t, firstUpdate, config.LastUpdated, "LastUpdated should not change due to throttling")

	// Update after throttle period should succeed and update timestamp
	// Note: We'll test this by temporarily reducing the throttle interval for this test
	// or by waiting the full 10+ seconds (but that makes tests too slow)
}

// TestJwksConfig_ThrottlingMechanism tests the new 10-second throttling mechanism
func TestJwksConfig_ThrottlingMechanism(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(`{"keys": [{"kty":"RSA","use":"sig","kid":"test-key-1","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`))
	}))
	defer server.Close()

	config := &JwksConfig{
		URL:    server.URL,
		Issuer: "test.example.com",
	}

	t.Run("Initial update should succeed", func(t *testing.T) {
		err := config.UpdateJWKS()
		require.NoError(t, err, "Initial update should succeed")
		assert.False(t, config.LastUpdated.IsZero(), "LastUpdated should be set after initial update")
	})

	t.Run("Rapid subsequent updates should be throttled", func(t *testing.T) {
		initialUpdate := config.LastUpdated

		// Multiple rapid updates should all be throttled
		for i := 0; i < 5; i++ {
			err := config.UpdateJWKS()
			assert.NoError(t, err, "Throttled update should not return error")
			assert.Equal(t, initialUpdate, config.LastUpdated, "LastUpdated should not change during throttle")
			time.Sleep(time.Millisecond) // Small delay between attempts
		}
	})

	t.Run("Concurrent update attempts should be handled safely", func(t *testing.T) {
		var wg sync.WaitGroup
		numGoroutines := 10

		for i := 0; i < numGoroutines; i++ {
			wg.Add(1)
			go func(goroutineID int) {
				defer wg.Done()
				err := config.UpdateJWKS()
				// Should not return error - either successful update or throttled
				assert.NoError(t, err, "Update should not error in goroutine %d", goroutineID)
			}(i)
		}

		wg.Wait()
		// All goroutines should complete without error or panic
	})

	t.Run("Update is allowed for fresh config", func(t *testing.T) {
		// Create a new config that has never been updated
		freshConfig := &JwksConfig{
			URL:    server.URL,
			Issuer: "test.example.com",
		}

		err := freshConfig.UpdateJWKS()
		assert.NoError(t, err, "Fresh config should allow update")
		assert.False(t, freshConfig.LastUpdated.IsZero(), "LastUpdated should be set")
	})
}

// TestGetKeyFromJWKS_NoKidWithAlgorithm tests the primary scenario where JWT has no kid but has algorithm
func TestGetKeyFromJWKS_NoKidWithAlgorithm(t *testing.T) {
	// Generate RSA and ECDSA keys for testing
	rsaPrivateKey, err := rsa.GenerateKey(rand.Reader, 2048)
	require.NoError(t, err, "Failed to generate RSA key")

	ecdsaPrivateKey, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	require.NoError(t, err, "Failed to generate ECDSA key")

	tests := []struct {
		name             string
		signingKey       interface{}
		signingAlgorithm jose.SignatureAlgorithm
		jwksKeys         []testKeyInfo
		tokenKid         string // empty string means no kid in token
		tokenAlgorithm   string
		shouldSucceed    bool
		expectedKeyId    string
		description      string
	}{
		{
			name:             "Single RSA key, no kid in token, RS256 algorithm",
			signingKey:       rsaPrivateKey,
			signingAlgorithm: jose.RS256,
			jwksKeys: []testKeyInfo{
				{key: rsaPrivateKey.Public(), keyId: "rsa-key-1", algorithm: "RS256", use: "sig"},
			},
			tokenKid:       "", // No kid in token
			tokenAlgorithm: "RS256",
			shouldSucceed:  true,
			expectedKeyId:  "rsa-key-1",
			description:    "Should find RSA key by algorithm when no kid provided",
		},
		{
			name:             "Single ECDSA key, no kid in token, ES256 algorithm",
			signingKey:       ecdsaPrivateKey,
			signingAlgorithm: jose.ES256,
			jwksKeys: []testKeyInfo{
				{key: ecdsaPrivateKey.Public(), keyId: "ec-key-1", algorithm: "ES256", use: "sig"},
			},
			tokenKid:       "",
			tokenAlgorithm: "ES256",
			shouldSucceed:  true,
			expectedKeyId:  "ec-key-1",
			description:    "Should find ECDSA key by algorithm when no kid provided",
		},
		{
			name:             "Multiple keys, no kid, algorithm matching",
			signingKey:       rsaPrivateKey,
			signingAlgorithm: jose.RS256,
			jwksKeys: []testKeyInfo{
				{key: ecdsaPrivateKey.Public(), keyId: "ec-key-1", algorithm: "ES256", use: "sig"},
				{key: rsaPrivateKey.Public(), keyId: "rsa-key-1", algorithm: "RS256", use: "sig"},
			},
			tokenKid:       "",
			tokenAlgorithm: "RS256",
			shouldSucceed:  true,
			expectedKeyId:  "rsa-key-1",
			description:    "Should find correct key by algorithm among multiple keys",
		},
		{
			name:             "No matching algorithm",
			signingKey:       rsaPrivateKey,
			signingAlgorithm: jose.RS256,
			jwksKeys: []testKeyInfo{
				{key: ecdsaPrivateKey.Public(), keyId: "ec-key-1", algorithm: "ES256", use: "sig"},
			},
			tokenKid:       "",
			tokenAlgorithm: "RS256",
			shouldSucceed:  false,
			description:    "Should fail when no keys match the required algorithm",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create JWT token without kid
			tokenString, err := createJWTWithGoJose(tt.signingKey, tt.signingAlgorithm, tt.tokenKid)
			require.NoError(t, err, "Failed to create JWT token")

			// Create mock JWKS server
			jwksServer := createMockJWKSServerWithKeys(t, tt.jwksKeys)
			defer jwksServer.Close()

			// Create and configure JWKS config
			jwksConfig := NewJwksConfig("test-config", jwksServer.URL, "test-issuer", TokenOriginKasSsa, false, nil, nil)
			err = jwksConfig.UpdateJWKS()
			require.NoError(t, err, "Failed to update JWKS")

			// Test token validation end-to-end (public API)
			token, err := jwksConfig.ValidateToken(tokenString, jwt.MapClaims{})

			if tt.shouldSucceed {
				assert.NoError(t, err, "Token validation should succeed: %s", tt.description)
				assert.NotNil(t, token, "Validated token should not be nil")
				assert.True(t, token.Valid, "Token should be valid")
			} else {
				assert.Error(t, err, "Token validation should fail: %s", tt.description)
			}
		})
	}
}

// testKeyInfo represents key information for testing
type testKeyInfo struct {
	key       interface{}
	keyId     string
	algorithm string
	use       string
}

// createJWTWithGoJose creates a JWT token using go-jose with optional kid
func createJWTWithGoJose(privateKey interface{}, algorithm jose.SignatureAlgorithm, kid string) (string, error) {
	// Create the payload claims as a map
	now := time.Now()
	claims := map[string]interface{}{
		"iss": "test-issuer",
		"sub": "test-subject",
		"aud": []string{"ngc"},
		"exp": now.Add(time.Hour).Unix(),
		"iat": now.Unix(),
		"access": []map[string]interface{}{
			{
				"type":    "group/ngc-test",
				"name":    "test-org",
				"actions": []string{"read", "write"},
			},
		},
	}

	// Convert claims to JSON payload
	payload, err := json.Marshal(claims)
	if err != nil {
		return "", err
	}

	// Create go-jose signer with optional kid header
	var signer jose.Signer
	if kid != "" {
		signerOptions := &jose.SignerOptions{}
		signerOptions = signerOptions.WithHeader("kid", kid)

		signer, err = jose.NewSigner(
			jose.SigningKey{Algorithm: algorithm, Key: privateKey},
			signerOptions,
		)
	} else {
		// Create signer without kid header
		signer, err = jose.NewSigner(
			jose.SigningKey{Algorithm: algorithm, Key: privateKey},
			nil,
		)
	}

	if err != nil {
		return "", err
	}

	// Sign the payload using go-jose
	jws, err := signer.Sign(payload)
	if err != nil {
		return "", err
	}

	// Return the compact serialization (standard JWT format)
	return jws.CompactSerialize()
}

// createMockJWKSServerWithKeys creates a mock JWKS server with multiple keys using go-jose
func createMockJWKSServerWithKeys(t *testing.T, keys []testKeyInfo) *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var jwkSet jose.JSONWebKeySet

		for _, keyInfo := range keys {
			jwk := jose.JSONWebKey{
				Key:       keyInfo.key,
				KeyID:     keyInfo.keyId,
				Algorithm: keyInfo.algorithm,
				Use:       keyInfo.use,
			}
			jwkSet.Keys = append(jwkSet.Keys, jwk)
		}

		jsonData, err := json.Marshal(jwkSet)
		require.NoError(t, err, "Failed to marshal JWKS with go-jose")

		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		w.Write(jsonData)
	}))
}

// createTokenWithBadKid creates a JWT token with a nonexistent kid for testing rate limiting
func createTokenWithBadKid(t *testing.T, privateKey *rsa.PrivateKey, badKid string) string {
	// Use the existing go-jose approach that's already used in this file
	tokenString, err := createJWTWithGoJose(privateKey, jose.RS256, badKid)
	require.NoError(t, err, "Failed to create token with bad kid")
	return tokenString
}

// TestGetScopes_Formats tests scope extraction from JWT claims
// Supports both OAuth 2.0 space-separated strings and OIDC array formats
func TestGetScopes_Formats(t *testing.T) {
	tests := []struct {
		name           string
		claims         jwt.MapClaims
		expectedScopes mapset.Set[string]
	}{
		// Space-separated string format (OAuth 2.0 RFC 6749)
		{
			name: "scope as space-separated string",
			claims: jwt.MapClaims{
				"scope": "openid profile email",
			},
			expectedScopes: mapset.NewSet("openid", "profile", "email"),
		},
		{
			name: "scope as single string",
			claims: jwt.MapClaims{
				"scope": "nico",
			},
			expectedScopes: mapset.NewSet("nico"),
		},

		// Array format (modern OIDC implementations)
		{
			name: "scope as array of strings",
			claims: jwt.MapClaims{
				"scope": []string{"openid", "profile", "email"},
			},
			expectedScopes: mapset.NewSet("openid", "profile", "email"),
		},
		{
			name: "scope as array of interfaces",
			claims: jwt.MapClaims{
				"scope": []interface{}{"read:data", "write:data"},
			},
			expectedScopes: mapset.NewSet("read:data", "write:data"),
		},

		// Alternative scope claim names
		{
			name: "scopes (plural) as array",
			claims: jwt.MapClaims{
				"scopes": []string{"api.read", "api.write"},
			},
			expectedScopes: mapset.NewSet("api.read", "api.write"),
		},
		{
			name: "scp claim (Azure AD style)",
			claims: jwt.MapClaims{
				"scp": "User.Read User.Write",
			},
			expectedScopes: mapset.NewSet("User.Read", "User.Write"),
		},

		// Priority: scope > scopes > scp
		{
			name: "scope takes priority over scopes and scp",
			claims: jwt.MapClaims{
				"scope":  "primary",
				"scopes": []string{"ignored"},
				"scp":    "also_ignored",
			},
			expectedScopes: mapset.NewSet("primary"),
		},

		// Empty/missing cases
		{
			name:           "no scope claim",
			claims:         jwt.MapClaims{"sub": "user"},
			expectedScopes: mapset.NewSet[string](),
		},
		{
			name: "empty scope string",
			claims: jwt.MapClaims{
				"scope": "",
			},
			expectedScopes: mapset.NewSet[string](),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := core.GetScopes(tt.claims)
			resultSet := mapset.NewSet(result...)
			assert.True(t, tt.expectedScopes.Equal(resultSet), "expected %v, got %v", tt.expectedScopes, resultSet)
		})
	}
}

// TestValidateScopes tests scope validation
func TestValidateScopes(t *testing.T) {
	// Config with issuer-level scopes
	config := &JwksConfig{
		Name:   "scope-test",
		Scopes: []string{"nico", "openid"}, // Requires BOTH scopes at issuer level
	}

	t.Run("scope as space-separated string matches", func(t *testing.T) {
		claims := jwt.MapClaims{
			"sub":   "user",
			"scope": "nico openid profile", // space-separated
		}
		err := config.ValidateScopes(claims)
		require.NoError(t, err)
	})

	t.Run("scope as array matches", func(t *testing.T) {
		claims := jwt.MapClaims{
			"sub":   "user",
			"scope": []string{"nico", "openid", "profile"}, // array
		}
		err := config.ValidateScopes(claims)
		require.NoError(t, err)
	})

	t.Run("scope as interface array matches", func(t *testing.T) {
		claims := jwt.MapClaims{
			"sub":   "user",
			"scope": []interface{}{"nico", "openid"}, // interface array
		}
		err := config.ValidateScopes(claims)
		require.NoError(t, err)
	})

	t.Run("missing required scope returns core.ErrInvalidScope", func(t *testing.T) {
		claims := jwt.MapClaims{
			"sub":   "user",
			"scope": "nico only", // missing "openid"
		}
		err := config.ValidateScopes(claims)
		assert.ErrorIs(t, err, core.ErrInvalidScope)
	})

	t.Run("no scopes configured accepts any token", func(t *testing.T) {
		noScopeConfig := &JwksConfig{
			Name: "no-scope-test",
			// No Scopes configured
		}
		claims := jwt.MapClaims{
			"sub": "user",
			// No scope claim at all
		}
		err := noScopeConfig.ValidateScopes(claims)
		require.NoError(t, err)
	})
}

// TestAudienceFormats tests that audience claims work with both string and array formats
// Note: golang-jwt v5's claims.GetAudience() handles both formats automatically
func TestAudienceFormats(t *testing.T) {
	t.Run("audience as single string", func(t *testing.T) {
		claims := jwt.MapClaims{
			"sub": "user",
			"aud": "api.example.com", // single string
		}
		aud, err := claims.GetAudience()
		require.NoError(t, err)
		assert.Equal(t, []string{"api.example.com"}, []string(aud))
	})

	t.Run("audience as array of strings", func(t *testing.T) {
		claims := jwt.MapClaims{
			"sub": "user",
			"aud": []string{"api1.example.com", "api2.example.com"}, // array
		}
		aud, err := claims.GetAudience()
		require.NoError(t, err)
		assert.Equal(t, []string{"api1.example.com", "api2.example.com"}, []string(aud))
	})

	t.Run("audience as array of interfaces", func(t *testing.T) {
		claims := jwt.MapClaims{
			"sub": "user",
			"aud": []interface{}{"api1.example.com", "api2.example.com"}, // interface array
		}
		aud, err := claims.GetAudience()
		require.NoError(t, err)
		assert.Equal(t, []string{"api1.example.com", "api2.example.com"}, []string(aud))
	})

	t.Run("missing audience returns empty", func(t *testing.T) {
		claims := jwt.MapClaims{
			"sub": "user",
			// No aud claim
		}
		aud, err := claims.GetAudience()
		require.NoError(t, err)
		assert.Empty(t, aud)
	})
}

// TestValidateAudiences tests audience validation
func TestValidateAudiences(t *testing.T) {
	t.Run("no_audiences_configured_passes", func(t *testing.T) {
		config := &JwksConfig{
			Name:      "test",
			Audiences: nil,
		}
		claims := jwt.MapClaims{"sub": "user"}
		err := config.ValidateAudience(claims)
		assert.NoError(t, err)
	})

	t.Run("token_matches_one_of_configured_audiences", func(t *testing.T) {
		config := &JwksConfig{
			Name:      "test",
			Audiences: []string{"nico-rest-api", "other-api"},
		}
		claims := jwt.MapClaims{"sub": "user", "aud": "nico-rest-api"}
		err := config.ValidateAudience(claims)
		assert.NoError(t, err)
	})

	t.Run("token_audience_array_matches_configured", func(t *testing.T) {
		config := &JwksConfig{
			Name:      "test",
			Audiences: []string{"nico-rest-api"},
		}
		claims := jwt.MapClaims{"sub": "user", "aud": []string{"other", "nico-rest-api"}}
		err := config.ValidateAudience(claims)
		assert.NoError(t, err)
	})

	t.Run("token_audience_exact_match_required", func(t *testing.T) {
		config := &JwksConfig{
			Name:      "test",
			Audiences: []string{"NICo-REST-API"},
		}
		// Different case should NOT match (exact string comparison)
		claims := jwt.MapClaims{"sub": "user", "aud": "nico-rest-api"}
		err := config.ValidateAudience(claims)
		assert.ErrorIs(t, err, core.ErrInvalidAudience)
	})

	t.Run("token_audience_does_not_match", func(t *testing.T) {
		config := &JwksConfig{
			Name:      "test",
			Audiences: []string{"nico-rest-api"},
		}
		claims := jwt.MapClaims{"sub": "user", "aud": "wrong-audience"}
		err := config.ValidateAudience(claims)
		assert.ErrorIs(t, err, core.ErrInvalidAudience)
	})

	t.Run("missing_audience_when_required_fails", func(t *testing.T) {
		config := &JwksConfig{
			Name:      "test",
			Audiences: []string{"nico-rest-api"},
		}
		claims := jwt.MapClaims{"sub": "user"}
		err := config.ValidateAudience(claims)
		assert.ErrorIs(t, err, core.ErrInvalidAudience)
	})
}

func TestGetOrgDataFromClaimMappingAudiences(t *testing.T) {
	tests := []struct {
		name       string
		config     *JwksConfig
		claims     jwt.MapClaims
		requestOrg string
		wantErr    error
	}{
		{
			name: "matching audience authorizes org",
			config: &JwksConfig{ClaimMappings: []ClaimMapping{{
				OrgName:   "acme",
				Roles:     []string{"TENANT_ADMIN"},
				Audiences: []string{"org-acme"},
			}}},
			claims:     jwt.MapClaims{"aud": []string{"issuer-audience", "org-acme"}},
			requestOrg: "acme",
		},
		{
			name: "wrong audience denies org",
			config: &JwksConfig{ClaimMappings: []ClaimMapping{{
				OrgName:   "acme",
				Roles:     []string{"TENANT_ADMIN"},
				Audiences: []string{"org-acme"},
			}}},
			claims:     jwt.MapClaims{"aud": "different-audience"},
			requestOrg: "acme",
			wantErr:    core.ErrInvalidAudience,
		},
		{
			name: "mapping without audiences remains allowed",
			config: &JwksConfig{ClaimMappings: []ClaimMapping{{
				OrgName: "acme",
				Roles:   []string{"TENANT_ADMIN"},
			}}},
			claims:     jwt.MapClaims{},
			requestOrg: "acme",
		},
		{
			name: "reserved dynamic org is rejected before audience",
			config: &JwksConfig{
				ClaimMappings: []ClaimMapping{{
					OrgAttribute:   "org",
					RolesAttribute: "roles",
					Audiences:      []string{"org-acme"},
				}},
				ReservedOrgNames: map[string]bool{"acme": true},
			},
			claims:     jwt.MapClaims{"org": "acme", "roles": []string{"TENANT_ADMIN"}, "aud": "different-audience"},
			requestOrg: "acme",
			wantErr:    core.ErrReservedOrgName,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			orgData, _, err := tt.config.GetOrgDataFromClaim(tt.claims, tt.requestOrg)
			if tt.wantErr != nil {
				assert.ErrorIs(t, err, tt.wantErr)
				assert.Nil(t, orgData)
				return
			}

			require.NoError(t, err)
			assert.Contains(t, orgData, strings.ToLower(tt.requestOrg))
		})
	}
}
