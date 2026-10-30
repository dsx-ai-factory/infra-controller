// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/rsa"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
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
			config := NewJwksConfig(tt.url, tt.issuer, TokenOriginKasSsa, false, nil, nil)
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
				// Mock response writes are best-effort if the client closes the connection.
				_, _ = w.Write([]byte(`{"keys": [{"kty":"RSA","use":"sig","kid":"test-key-1","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`))
			},
			expectError: false,
		},
		{
			name: "invalid_json_response",
			serverResponse: func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(http.StatusOK)
				_, _ = w.Write([]byte(`{"keys": [invalid json`))
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
		// A token without a kid takes the alg-only lookup path, which must wait for
		// an in-flight fetch exactly as the kid path does.
		tests := []struct {
			name       string
			warmCache  bool
			withoutKid bool
		}{
			{name: "cold cache"},
			{name: "warm cache", warmCache: true},
			{name: "cold cache, token without kid", withoutKid: true},
			{name: "warm cache, token without kid", warmCache: true, withoutKid: true},
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

				tokenString, err := createTokenWithGoJose(privateKey, !tt.withoutKid, "new-key-id")
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
					token, validationErr := config.ValidateTokenContext(context.Background(), tokenString, jwt.MapClaims{})
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
		_, _ = w.Write([]byte(`{
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
		_, _ = w.Write([]byte(`{"keys": [{"kty":"RSA","use":"sig","kid":"test-key-1","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`))
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
		_, _ = w.Write([]byte(`{"keys": [{"kty":"RSA","use":"sig","kid":"test-key-1","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`))
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
			jwksConfig := NewJwksConfig(jwksServer.URL, "test-issuer", TokenOriginKasSsa, false, nil, nil)
			err = jwksConfig.UpdateJWKS()
			require.NoError(t, err, "Failed to update JWKS")

			// Test token validation end-to-end (public API)
			token, err := jwksConfig.ValidateTokenContext(context.Background(), tokenString, jwt.MapClaims{})

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

// TestJwksConfig_ValidateTokenRequiresExpiration guards that a correctly
// signed token without exp is rejected, rather than staying valid until its
// signing key is rotated out.
func TestJwksConfig_ValidateTokenRequiresExpiration(t *testing.T) {
	privateKey, err := rsa.GenerateKey(rand.Reader, 2048)
	require.NoError(t, err)
	jwksServer := createMockJWKSServerWithKeys(t, []testKeyInfo{
		{key: privateKey.Public(), keyId: "rsa-key-1", algorithm: "RS256", use: "sig"},
	})
	defer jwksServer.Close()
	jwksConfig := NewJwksConfig("test-config", jwksServer.URL, "test-issuer", TokenOriginKasSsa, false, nil, nil)
	require.NoError(t, jwksConfig.UpdateJWKS())

	tests := []struct {
		name    string
		claims  jwt.MapClaims
		wantErr error
	}{
		{
			name:    "signed token without exp is rejected",
			claims:  jwt.MapClaims{"iss": "test-issuer", "sub": "test-subject"},
			wantErr: jwt.ErrTokenRequiredClaimMissing,
		},
		{
			name:   "signed token with a future exp is accepted",
			claims: jwt.MapClaims{"iss": "test-issuer", "sub": "test-subject", "exp": time.Now().Add(time.Hour).Unix()},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			unsigned := jwt.NewWithClaims(jwt.SigningMethodRS256, tt.claims)
			unsigned.Header["kid"] = "rsa-key-1"
			tokenString, err := unsigned.SignedString(privateKey)
			require.NoError(t, err)

			_, err = jwksConfig.ValidateToken(tokenString, jwt.MapClaims{})
			if tt.wantErr != nil {
				require.ErrorIs(t, err, tt.wantErr)
			} else {
				require.NoError(t, err)
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
		_, _ = w.Write(jsonData)
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
			Audiences: nil,
		}
		claims := jwt.MapClaims{"sub": "user"}
		err := config.ValidateAudience(claims)
		assert.NoError(t, err)
	})

	t.Run("token_matches_one_of_configured_audiences", func(t *testing.T) {
		config := &JwksConfig{
			Audiences: []string{"nico-rest-api", "other-api"},
		}
		claims := jwt.MapClaims{"sub": "user", "aud": "nico-rest-api"}
		err := config.ValidateAudience(claims)
		assert.NoError(t, err)
	})

	t.Run("token_audience_array_matches_configured", func(t *testing.T) {
		config := &JwksConfig{
			Audiences: []string{"nico-rest-api"},
		}
		claims := jwt.MapClaims{"sub": "user", "aud": []string{"other", "nico-rest-api"}}
		err := config.ValidateAudience(claims)
		assert.NoError(t, err)
	})

	t.Run("token_audience_exact_match_required", func(t *testing.T) {
		config := &JwksConfig{
			Audiences: []string{"NICo-REST-API"},
		}
		// Different case should NOT match (exact string comparison)
		claims := jwt.MapClaims{"sub": "user", "aud": "nico-rest-api"}
		err := config.ValidateAudience(claims)
		assert.ErrorIs(t, err, core.ErrInvalidAudience)
	})

	t.Run("token_audience_does_not_match", func(t *testing.T) {
		config := &JwksConfig{
			Audiences: []string{"nico-rest-api"},
		}
		claims := jwt.MapClaims{"sub": "user", "aud": "wrong-audience"}
		err := config.ValidateAudience(claims)
		assert.ErrorIs(t, err, core.ErrInvalidAudience)
	})

	t.Run("missing_audience_when_required_fails", func(t *testing.T) {
		config := &JwksConfig{
			Audiences: []string{"nico-rest-api"},
		}
		claims := jwt.MapClaims{"sub": "user"}
		err := config.ValidateAudience(claims)
		assert.ErrorIs(t, err, core.ErrInvalidAudience)
	})
}

func TestGetOrgDataFromClaimMappingFilters(t *testing.T) {
	tests := []struct {
		name       string
		config     *JwksConfig
		reserved   map[string]bool
		claims     jwt.MapClaims
		requestOrg string
		wantRoles  []string
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
			name: "mapping without audience or scope filters remains allowed",
			config: &JwksConfig{ClaimMappings: []ClaimMapping{{
				OrgName: "acme",
				Roles:   []string{"TENANT_ADMIN"},
			}}},
			claims:     jwt.MapClaims{},
			requestOrg: "acme",
		},
		{
			name: "token carrying every required scope authorizes org",
			config: &JwksConfig{ClaimMappings: []ClaimMapping{{
				OrgName: "acme",
				Roles:   []string{"TENANT_ADMIN"},
				Scopes:  []string{"nico:read", "nico:write"},
			}}},
			claims:     jwt.MapClaims{"scope": "openid nico:read nico:write"},
			requestOrg: "acme",
		},
		{
			name: "token missing one required scope denies org",
			config: &JwksConfig{ClaimMappings: []ClaimMapping{{
				OrgName: "acme",
				Roles:   []string{"TENANT_ADMIN"},
				Scopes:  []string{"nico:read", "nico:write"},
			}}},
			claims:     jwt.MapClaims{"scope": "openid nico:read"},
			requestOrg: "acme",
			wantErr:    core.ErrInvalidScope,
		},
		{
			name: "token with no scope claim denies a scoped org",
			config: &JwksConfig{ClaimMappings: []ClaimMapping{{
				OrgName: "acme",
				Roles:   []string{"TENANT_ADMIN"},
				Scopes:  []string{"nico:read"},
			}}},
			claims:     jwt.MapClaims{},
			requestOrg: "acme",
			wantErr:    core.ErrInvalidScope,
		},
		{
			name: "scopes only gate the org that requires them",
			config: &JwksConfig{ClaimMappings: []ClaimMapping{
				{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}, Scopes: []string{"nico:read"}},
				{OrgName: "globex", Roles: []string{"TENANT_ADMIN"}},
			}},
			claims:     jwt.MapClaims{},
			requestOrg: "globex",
		},
		{
			name: "audience is rejected before scopes",
			config: &JwksConfig{ClaimMappings: []ClaimMapping{{
				OrgName:   "acme",
				Roles:     []string{"TENANT_ADMIN"},
				Audiences: []string{"org-acme"},
				Scopes:    []string{"nico:read"},
			}}},
			claims:     jwt.MapClaims{"aud": "different-audience"},
			requestOrg: "acme",
			wantErr:    core.ErrInvalidAudience,
		},
		{
			name: "reserved dynamic org is rejected before audience",
			config: &JwksConfig{
				ClaimMappings: []ClaimMapping{{
					OrgAttribute:   "org",
					RolesAttribute: "roles",
					Audiences:      []string{"org-acme"},
				}},
			},
			reserved:   map[string]bool{"acme": true},
			claims:     jwt.MapClaims{"org": "acme", "roles": []string{"TENANT_ADMIN"}, "aud": "different-audience"},
			requestOrg: "acme",
			wantErr:    core.ErrReservedOrgName,
		},
		{
			name: "static mapping wins over a reserved dynamic claim of the same org",
			config: &JwksConfig{ClaimMappings: []ClaimMapping{
				{OrgName: "acme", Roles: []string{"PROVIDER_ADMIN"}},
				{OrgAttribute: "org", RolesAttribute: "roles"},
			}},
			reserved:   map[string]bool{"acme": true},
			claims:     jwt.MapClaims{"org": "acme", "roles": []string{"TENANT_ADMIN"}},
			requestOrg: "acme",
			wantRoles:  []string{"PROVIDER_ADMIN"},
		},
		{
			name: "static mapping wins regardless of mapping order",
			config: &JwksConfig{ClaimMappings: []ClaimMapping{
				{OrgAttribute: "org", RolesAttribute: "roles"},
				{OrgName: "acme", Roles: []string{"PROVIDER_ADMIN"}},
			}},
			reserved:   map[string]bool{"acme": true},
			claims:     jwt.MapClaims{"org": "acme", "roles": []string{"TENANT_ADMIN"}},
			requestOrg: "acme",
			wantRoles:  []string{"PROVIDER_ADMIN"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.reserved != nil {
				tt.config.SetReservedOrgNames(tt.reserved)
			}
			orgData, _, err := tt.config.GetOrgDataFromClaim(tt.claims, tt.requestOrg)
			if tt.wantErr != nil {
				assert.ErrorIs(t, err, tt.wantErr)
				assert.Nil(t, orgData)
				return
			}

			require.NoError(t, err)
			require.Contains(t, orgData, strings.ToLower(tt.requestOrg))
			if tt.wantRoles != nil {
				assert.Equal(t, tt.wantRoles, orgData[strings.ToLower(tt.requestOrg)].Roles)
			}
		})
	}
}

const cachedKeySetJSON = `{"keys":[{"kty":"RSA","use":"sig","kid":"cached-key","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`

// rotatedKeySetJSON stands in for the key set an issuer publishes after a rotation,
// so a test can tell which of two competing installs won.
const rotatedKeySetJSON = `{"keys":[{"kty":"RSA","use":"sig","kid":"rotated-key","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`

// countingJWKSServer serves a fixed key set and counts how many times it was hit,
// which is how the anti-stall tests assert that concurrent misses collapse into
// one request rather than one per caller.
func countingJWKSServer(t *testing.T, delay time.Duration) (*httptest.Server, *atomic.Int32) {
	t.Helper()

	var hits atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		if delay > 0 {
			time.Sleep(delay)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(cachedKeySetJSON))
	}))
	t.Cleanup(server.Close)

	return server, &hits
}

func TestSetJWKSFromCache(t *testing.T) {
	jwks, err := core.NewJWKSFromBytes([]byte(cachedKeySetJSON))
	require.NoError(t, err)

	server, hits := countingJWKSServer(t, 0)
	cfg := &JwksConfig{URL: server.URL, Issuer: "https://idp.example.com"}

	fetchedAt := time.Now().Add(-time.Second)
	cfg.SetJWKSFromCache(jwks, fetchedAt)

	assert.Equal(t, 1, cfg.KeyCount(), "hydration installs the cached keys")
	assert.Equal(t, fetchedAt, cfg.LastFetchedAt(), "hydration seeds the throttle with the cached fetch time")
	assert.Zero(t, hits.Load(), "hydration must not touch the network")

	// The seeded timestamp is recent, so the throttle must suppress a refresh: a
	// restarting replica should not stampede the IdP for keys it already has.
	fetched, rerr := cfg.RefreshJWKS(context.Background())
	require.NoError(t, rerr)
	assert.False(t, fetched, "a recently cached key set must not trigger an immediate refetch")
	assert.Zero(t, hits.Load())
}

// TestFailedFetchRetriesOnTheNextLoopTick: the issuer refresh loop retries a
// failed fetch every 10s tick. Neither tick jitter nor adopting a peer's key set
// in between may let the throttle suppress that retry.
func TestFailedFetchRetriesOnTheNextLoopTick(t *testing.T) {
	cached, err := core.NewJWKSFromBytes([]byte(cachedKeySetJSON))
	require.NoError(t, err)
	const tick = 10 * time.Second

	tests := []struct {
		name    string
		between func(cfg *JwksConfig)
	}{
		{name: "next tick fires a little early"},
		{name: "a peer's key set was adopted after the failure", between: func(cfg *JwksConfig) {
			cfg.SetJWKSFromCache(cached, time.Now().Add(-time.Second))
		}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server, hits := countingJWKSServer(t, 0)
			cfg := &JwksConfig{URL: server.URL, Issuer: "https://idp.example.com"}
			cfg.Lock()
			cfg.LastAttempted = time.Now().Add(-(tick - 50*time.Millisecond)) // the failed attempt
			cfg.Unlock()
			if tt.between != nil {
				tt.between(cfg)
			}

			fetched, err := cfg.RefreshJWKS(context.Background())
			require.NoError(t, err)
			assert.True(t, fetched, "the retry one tick later is not throttled")
			assert.Equal(t, int32(1), hits.Load())
		})
	}
}

func TestRefreshJWKSDistinguishesThrottledFromFetched(t *testing.T) {
	server, hits := countingJWKSServer(t, 0)
	cfg := &JwksConfig{URL: server.URL, Issuer: "https://idp.example.com"}

	fetched, err := cfg.RefreshJWKS(context.Background())
	require.NoError(t, err)
	assert.True(t, fetched, "the first refresh actually fetches")
	assert.Equal(t, int32(1), hits.Load())

	fetched, err = cfg.RefreshJWKS(context.Background())
	require.NoError(t, err)
	assert.False(t, fetched, "a refresh inside the throttle window reports that it did nothing")
	assert.Equal(t, int32(1), hits.Load(), "the throttled call must not reach the IdP")
}

// TestExpiredKeySetIsRefreshedBeforeUse covers the ceiling on cached trust: a
// snapshot older than maxJWKSAge cannot validate tokens on its own, and the miss it
// produces drives the refresh that makes the issuer usable again.
func TestExpiredKeySetIsRefreshedBeforeUse(t *testing.T) {
	jwks, err := core.NewJWKSFromBytes([]byte(cachedKeySetJSON))
	require.NoError(t, err)

	server, hits := countingJWKSServer(t, 0)
	cfg := &JwksConfig{URL: server.URL, Issuer: "https://idp.example.com"}
	cfg.SetJWKSFromCache(jwks, time.Now().Add(-maxJWKSAge-time.Minute))

	assert.Nil(t, cfg.GetJWKS(), "a key set past maxJWKSAge is no longer usable")
	_, kerr := cfg.GetKeyByID("cached-key")
	assert.ErrorIs(t, kerr, core.ErrJWKSExpired)

	token := &jwt.Token{Header: map[string]interface{}{"alg": "RS256", "kid": "cached-key"}}
	_, _ = cfg.getPublicKey(context.Background(), token)

	assert.Equal(t, int32(1), hits.Load(), "an expired key set is re-fetched on the request path")
	assert.NotNil(t, cfg.GetJWKS(), "the confirmed key set is trusted again")
}

// TestExpiredKeySetFailsClosedWhileIssuerIsUnreachable is the security half of the
// same policy: availability during an issuer outage stops at maxJWKSAge instead of
// continuing indefinitely on keys the issuer may have retired.
func TestExpiredKeySetFailsClosedWhileIssuerIsUnreachable(t *testing.T) {
	jwks, err := core.NewJWKSFromBytes([]byte(cachedKeySetJSON))
	require.NoError(t, err)

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	t.Cleanup(server.Close)

	cfg := &JwksConfig{URL: server.URL, Issuer: "https://idp.example.com"}
	cfg.SetJWKSFromCache(jwks, time.Now().Add(-maxJWKSAge-time.Minute))

	token := &jwt.Token{Header: map[string]interface{}{"alg": "RS256", "kid": "cached-key"}}
	_, kerr := cfg.getPublicKey(context.Background(), token)

	require.Error(t, kerr, "keys the issuer has not confirmed within maxJWKSAge must not validate tokens")
	assert.Equal(t, 1, cfg.KeyCount(), "the expired set is kept for the next refresh attempt, not evicted")
}

// TestInstallRejectsAnOlderKeySet covers the stale-write race persistence makes
// possible: a fetch that started before another replica's newer snapshot was
// hydrated must not roll this replica back to the older keys and reject tokens
// signed by the new ones until the next reload.
func TestInstallRejectsAnOlderKeySet(t *testing.T) {
	older, err := core.NewJWKSFromBytes([]byte(cachedKeySetJSON))
	require.NoError(t, err)
	newer, err := core.NewJWKSFromBytes([]byte(rotatedKeySetJSON))
	require.NoError(t, err)

	slowFetchStartedAt := time.Now().Add(-time.Minute)
	hydratedAt := slowFetchStartedAt.Add(30 * time.Second)

	cfg := &JwksConfig{URL: "https://idp.example.com/jwks", Issuer: "https://idp.example.com"}
	cfg.SetJWKSFromCache(newer, hydratedAt)
	cfg.install(older, slowFetchStartedAt, true)

	live := cfg.GetJWKS()
	require.NotNil(t, live)
	require.Len(t, live.Set.Keys, 1)
	assert.Equal(t, "rotated-key", live.Set.Keys[0].KeyID,
		"the newer key set survives a late-completing older fetch")
	assert.Equal(t, hydratedAt, cfg.LastFetchedAt())
}

// TestConcurrentKIDMissesProduceOneFetch is the core anti-stall guarantee: a burst
// of requests all missing on the same unknown kid must cost one HTTP fetch, not
// one per request.
func TestConcurrentKIDMissesProduceOneFetch(t *testing.T) {
	server, hits := countingJWKSServer(t, 50*time.Millisecond)
	cfg := &JwksConfig{URL: server.URL, Issuer: "https://idp.example.com"}

	const callers = 20
	var wg sync.WaitGroup
	for i := 0; i < callers; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			token := &jwt.Token{Header: map[string]interface{}{"alg": "RS256", "kid": "unknown-kid"}}
			_, _ = cfg.getPublicKey(context.Background(), token)
		}()
	}
	wg.Wait()

	assert.Equal(t, int32(1), hits.Load(), "concurrent misses must collapse into a single fetch")
}

// TestSharedFlightInstallsOnEveryWaiter covers the case the URL-keyed flight
// makes possible: the config that joins a fetch started by a different config
// (a registry rebuild mid-flight, or two issuers sharing a JWKS endpoint) must
// still end up with the keys installed on itself.
func TestSharedFlightInstallsOnEveryWaiter(t *testing.T) {
	server, hits := countingJWKSServer(t, 50*time.Millisecond)

	first := &JwksConfig{URL: server.URL, Issuer: "https://idp-a.example.com"}
	second := &JwksConfig{URL: server.URL, Issuer: "https://idp-b.example.com"}

	var wg sync.WaitGroup
	for _, cfg := range []*JwksConfig{first, second} {
		wg.Add(1)
		go func(cfg *JwksConfig) {
			defer wg.Done()
			fetched, err := cfg.RefreshJWKS(context.Background())
			assert.NoError(t, err)
			assert.True(t, fetched)
		}(cfg)
	}
	wg.Wait()

	assert.Equal(t, int32(1), hits.Load(), "the two configs share one fetch")
	assert.Equal(t, 1, first.KeyCount())
	assert.Equal(t, 1, second.KeyCount(), "a waiter must install the shared result on itself")
}

// TestUnknownKIDIsNegativeCached covers the sequential case singleflight cannot:
// once a fresh key set has been seen without the kid, later requests carrying it
// must be rejected without another fetch.
func TestUnknownKIDIsNegativeCached(t *testing.T) {
	server, hits := countingJWKSServer(t, 0)
	cfg := &JwksConfig{URL: server.URL, Issuer: "https://idp.example.com"}

	token := &jwt.Token{Header: map[string]interface{}{"alg": "RS256", "kid": "unknown-kid"}}

	_, err := cfg.getPublicKey(context.Background(), token)
	require.Error(t, err)
	require.Equal(t, int32(1), hits.Load())

	assert.True(t, cfg.isUnknownKID("unknown-kid"), "a kid absent from a fresh key set is remembered")

	_, err = cfg.getPublicKey(context.Background(), token)
	require.Error(t, err)
	assert.Equal(t, int32(1), hits.Load(), "a negative-cached kid must not drive another fetch")
}

// TestFailedFetchIsThrottledAndNegativeCached covers the case a healthy server
// cannot: when the IdP is down, sequential requests carrying an unknown kid must
// cost one outbound fetch, not one per request.
func TestFailedFetchIsThrottledAndNegativeCached(t *testing.T) {
	closed := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {}))
	closed.Close()

	cases := []struct {
		name  string
		serve func(t *testing.T) (string, *atomic.Int32)
	}{
		{name: "http 500", serve: func(t *testing.T) (string, *atomic.Int32) {
			var hits atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				hits.Add(1)
				w.WriteHeader(http.StatusInternalServerError)
			}))
			t.Cleanup(server.Close)
			return server.URL, &hits
		}},
		{name: "connection refused", serve: func(t *testing.T) (string, *atomic.Int32) {
			return closed.URL, &atomic.Int32{}
		}},
		{name: "timeout", serve: func(t *testing.T) (string, *atomic.Int32) {
			var hits atomic.Int32
			release := make(chan struct{})
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				hits.Add(1)
				<-release
			}))
			t.Cleanup(func() { close(release); server.Close() })
			return server.URL, &hits
		}},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			url, hits := tc.serve(t)
			cfg := &JwksConfig{URL: url, Issuer: "https://idp.example.com", JWKSTimeout: 50 * time.Millisecond}

			first := &jwt.Token{Header: map[string]interface{}{"alg": "RS256", "kid": "kid-a"}}
			_, err := cfg.getPublicKey(context.Background(), first)
			require.Error(t, err)
			assert.True(t, cfg.isUnknownKID("kid-a"), "a kid whose fetch failed is remembered")
			cfg.RLock()
			attempted := cfg.LastAttempted
			cfg.RUnlock()
			assert.Less(t, time.Since(attempted), minUpdateInterval, "a failed fetch must still arm the throttle")

			for i := 0; i < 5; i++ {
				kid := fmt.Sprintf("kid-%d", i)
				_, err = cfg.getPublicKey(context.Background(), &jwt.Token{Header: map[string]interface{}{"alg": "RS256", "kid": kid}})
				require.Error(t, err)
			}
			assert.LessOrEqual(t, hits.Load(), int32(1), "sequential misses against a failing IdP must share one fetch per interval")
			assert.True(t, cfg.LastUpdated.IsZero(), "a failed fetch must not count as a successful update")
		})
	}
}

// TestNegativeCacheClearedOnNewKeySet proves the cache cannot outlive the key set
// it was derived from, so a key the IdP publishes later is still picked up.
func TestNegativeCacheClearedOnNewKeySet(t *testing.T) {
	jwks, err := core.NewJWKSFromBytes([]byte(cachedKeySetJSON))
	require.NoError(t, err)

	cfg := &JwksConfig{URL: "https://idp.example.com/jwks", Issuer: "https://idp.example.com"}
	cfg.rememberUnknownKID("rotated-in-key")
	require.True(t, cfg.isUnknownKID("rotated-in-key"))

	cfg.SetJWKSFromCache(jwks, time.Now())
	assert.False(t, cfg.isUnknownKID("rotated-in-key"), "installing a new key set invalidates the negative cache")
}

func TestNegativeCacheIsBounded(t *testing.T) {
	cfg := &JwksConfig{URL: "https://idp.example.com/jwks", Issuer: "https://idp.example.com"}

	for i := 0; i < maxNegativeKIDs*3; i++ {
		cfg.rememberUnknownKID(string(rune(i)))
	}

	cfg.RLock()
	defer cfg.RUnlock()
	assert.LessOrEqual(t, len(cfg.negativeKIDs), maxNegativeKIDs,
		"an attacker minting random kids must not be able to grow the cache without limit")
}

// TestRefreshJWKSHonorsContext guards against the old sleep-based retry loop
// returning: a caller that has given up must not be held by a hung IdP.
func TestRefreshJWKSHonorsContext(t *testing.T) {
	release := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		<-release
	}))
	defer server.Close()
	defer close(release)

	cfg := &JwksConfig{URL: server.URL, Issuer: "https://idp.example.com", JWKSTimeout: 30 * time.Second}

	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()

	start := time.Now()
	fetched, err := cfg.RefreshJWKS(ctx)
	elapsed := time.Since(start)

	require.Error(t, err)
	assert.False(t, fetched)
	assert.Less(t, elapsed, 5*time.Second, "a cancelled request must abort promptly")
}

// TestRefreshJWKSStampsFetchStart pins the timestamp semantics the persisted
// cache's stale-write guard depends on: the key set is stamped with the time its
// request was issued, not the time it completed, so a slow fetch that started
// first can never look newer than one that started later.
func TestRefreshJWKSStampsFetchStart(t *testing.T) {
	const delay = 300 * time.Millisecond
	server, _ := countingJWKSServer(t, delay)
	cfg := &JwksConfig{URL: server.URL, Issuer: "https://idp.example.com"}

	fetched, err := cfg.RefreshJWKS(context.Background())
	completedAt := time.Now().UTC()

	require.NoError(t, err)
	require.True(t, fetched)
	assert.GreaterOrEqual(t, completedAt.Sub(cfg.LastFetchedAt()), delay,
		"the installed stamp predates the response by at least the time the IdP took")
}

// TestRefreshJWKSWaiterCancellationIsIndependent covers the two halves of the
// singleflight contract: a waiter that gives up returns on its own context, and
// its departure neither cancels nor is required by the shared fetch.
func TestRefreshJWKSWaiterCancellationIsIndependent(t *testing.T) {
	entered := make(chan struct{}, 1)
	release := make(chan struct{})
	releaseOnce := sync.OnceFunc(func() { close(release) })

	var hits atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		select {
		case entered <- struct{}{}:
		default:
		}
		<-release
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(cachedKeySetJSON))
	}))
	// Cleanups run last-registered-first, so the handlers are released before Close
	// waits on them.
	t.Cleanup(server.Close)
	t.Cleanup(releaseOnce)

	leader := &JwksConfig{URL: server.URL, Issuer: "https://idp-a.example.com", JWKSTimeout: 30 * time.Second}
	waiter := &JwksConfig{URL: server.URL, Issuer: "https://idp-b.example.com", JWKSTimeout: 30 * time.Second}

	leaderDone := make(chan error, 1)
	go func() {
		_, lerr := leader.RefreshJWKS(context.Background())
		leaderDone <- lerr
	}()
	<-entered

	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	start := time.Now()
	fetched, err := waiter.RefreshJWKS(ctx)

	require.Error(t, err, "a waiter whose caller gave up must not wait for the flight")
	assert.False(t, fetched)
	assert.Less(t, time.Since(start), time.Second)
	assert.Equal(t, 0, waiter.KeyCount(), "the departed waiter installs nothing")

	releaseOnce()
	require.NoError(t, <-leaderDone, "the shared fetch survives the waiter's cancellation")
	assert.Equal(t, 1, leader.KeyCount())
	assert.Equal(t, int32(1), hits.Load(), "both callers shared one request")
}

// TestFutureStampFromAPeerIsClamped covers a replica whose clock runs ahead
// persisting a key set: hydrating its stamp as is would suppress this replica's
// fetches, refuse every local install, and extend maxJWKSAge by the skew.
func TestFutureStampFromAPeerIsClamped(t *testing.T) {
	server, hits := countingJWKSServer(t, 0)
	cached, err := core.NewJWKSFromBytes([]byte(cachedKeySetJSON))
	require.NoError(t, err)

	cfg := &JwksConfig{URL: server.URL, Issuer: "https://idp.example.com"}
	cfg.SetJWKSFromCache(cached, time.Now().Add(time.Hour))
	assert.False(t, cfg.LastFetchedAt().After(time.Now()), "a peer's future stamp is clamped to now")

	// Make the clamped stamp old enough to clear the throttle, then a local fetch
	// must still run and still install.
	cfg.Lock()
	cfg.LastUpdated = time.Now().Add(-time.Minute)
	cfg.LastAttempted = cfg.LastUpdated
	cfg.Unlock()

	fetched, err := cfg.RefreshJWKS(context.Background())
	require.NoError(t, err)
	assert.True(t, fetched, "a local fetch is installed once the stamp is in the past")
	assert.Equal(t, int32(1), hits.Load())
}

// TestRefreshReportsARefusedInstall pins the fetched flag to "the installed set
// is new": a fetch the monotonic guard refuses must not be reported as fresh, or
// the caller retries a lookup that cannot succeed and persists keys it never
// installed.
func TestRefreshReportsARefusedInstall(t *testing.T) {
	server, _ := countingJWKSServer(t, 0)
	rotated, err := core.NewJWKSFromBytes([]byte(rotatedKeySetJSON))
	require.NoError(t, err)

	cfg := &JwksConfig{URL: server.URL, Issuer: "https://idp.example.com"}
	// Installed "just now", so a fetch issued a moment later is newer and installs.
	fetched, err := cfg.RefreshJWKS(context.Background())
	require.NoError(t, err)
	require.True(t, fetched)

	// A set stamped later than any fetch this test can issue: the next fetch's
	// start time is not after it, so install refuses the fetched set.
	cfg.Lock()
	cfg.LastAttempted = time.Now().Add(-time.Minute) // clear the throttle
	cfg.Unlock()
	cfg.install(rotated, time.Now().Add(time.Second), true)

	fetched, err = cfg.RefreshJWKS(context.Background())
	require.NoError(t, err)
	assert.False(t, fetched, "a refused install is not reported as a fresh set")
	live := cfg.GetJWKS()
	require.NotNil(t, live)
	assert.Equal(t, "rotated-key", live.Set.Keys[0].KeyID, "the newer set stays installed")
}

// TestLastAttemptReportsSettledOutcomesOnly: a fetch claims its attempt time
// before the network call, so an observer that reads it mid-flight beside the
// previous outcome would record a result the fetch has not produced.
func TestLastAttemptReportsSettledOutcomesOnly(t *testing.T) {
	release := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		<-release
		_, _ = w.Write([]byte(cachedKeySetJSON))
	}))
	defer server.Close()
	cfg := NewJwksConfig(server.URL, "https://idp.example.com", TokenOriginCustom, false, nil, nil)

	done := make(chan struct{})
	go func() { _, _ = cfg.RefreshJWKS(context.Background()); close(done) }()
	require.Eventually(t, func() bool { cfg.RLock(); defer cfg.RUnlock(); return cfg.updateDone != nil }, time.Second, time.Millisecond)
	at, _ := cfg.LastAttempt()
	assert.True(t, at.IsZero(), "an attempt in flight is not an outcome")

	close(release)
	<-done
	at, err := cfg.LastAttempt()
	assert.False(t, at.IsZero())
	assert.NoError(t, err)
}

// TestGetOrgDataFromClaimNamesEveryMiss: when one mapping of the org refuses the
// token's audience and another its scopes, the error names both, so the caller
// is not told to fix scopes that the only audience-compatible mapping does not ask for.
func TestGetOrgDataFromClaimNamesEveryMiss(t *testing.T) {
	cfg := &JwksConfig{ClaimMappings: []ClaimMapping{
		{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}, Audiences: []string{"org-acme"}},
		{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}, Audiences: []string{"other"}, Scopes: []string{"nico:read"}},
	}}

	_, _, err := cfg.GetOrgDataFromClaim(jwt.MapClaims{"aud": "other"}, "acme")

	assert.ErrorIs(t, err, core.ErrInvalidAudience)
	assert.ErrorIs(t, err, core.ErrInvalidScope)
}
