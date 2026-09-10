// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"crypto/rand"
	"crypto/rsa"
	"encoding/base64"
	"encoding/json"
	"math/big"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/go-jose/go-jose/v4"
	"github.com/golang-jwt/jwt/v5"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

var (
	legacyKasJwks = `{"keys":[{"kty":"RSA","e":"AQAB","use":"sig","kid":"B3D2:ZQHB:M5YX:NFBJ:DFI4:U4WX:PH5E:4JXH:CNAY:WYTJ:AZWC:RALK","n":"qcqYV-iYUV6JNcMh58qLlMt8d_6AzkJcqlR77hUzR-fismhwoerT2K9vIBOno30mjKsgJjoT4zhPA8q28Sqq_AMWh7wqoBr99O75YdUawjfcngvHKCvfihN2E1Z4f-C8ihtn8T6rh9VcldLDaEhUlCIisRBTY3lnw4recPKE-cC0ejgFeOnV5Ds5a_xb1sP9Dhwv_hqIR_1Khh_H6M6WfF3Tv3eAgMQWycjCQkAY47qwXi9DCkAOhJJwlP0djsHPYKfykMKe5MUfnbPE-bCYg7rQlZfdzd58zL2G9VUyOLzZtFhGwPCA6oRyqlKTKO1dN0_wjMXa_86L0GswW-etl0HRL1KlP8ctF1m99xQ3M5leE8JOeio0eUPJNLgssClxHEW75JSXYB6T8YJek41FjQttW2sZpw1L-iQYLWVA5bIx7QEqcu85EmQQik4mvq_azX53Mug6_5tJPitdox_LQf38RIANa5zhPYcwqObjTr8W0rxMjXFN0bRrZ5f_RaXqbSdh5vVWmdzsZu0xu0otujz50ZlR5rf0W5leTs1xTLwpHh1CC2jhThwcOFkXT46zqWaKE7rsik3bp79yKHA9wkqzQOK4TE_DGp8aPrfa_8CAR1iVkbpW4diHgV-XuHLhFFjQco3I6SzPt4Ael_JoldaH2bINKvPaJXKCi_Bm9L8"}]}`
	ssaJwks       = `{"keys":[{"kty":"EC","use":"sig","crv":"P-256","kid":"2c58e180-149a-4818-9bfc-5f2a6b6dbd8a","x":"d4Sa5NYfomfkYkSdQEUrTKHXEET2dNhyQVnEViA97L0","y":"dQTndo4VhAy1G3i0Z9V6tEq7Ii2ey59pAM-GFoaI5M8","alg":"ES256"}]}`
)

func TestNewTokenOriginConfig(t *testing.T) {
	// Generate a test server so we can capture and inspect the request
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/kas") {
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(legacyKasJwks))
		} else if strings.Contains(req.URL.Path, "/ssa") {
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(ssaJwks))
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer func() { testServer.Close() }()

	type args struct {
		legacyJwksURL string
		ssaJwksURL    string
	}
	tests := []struct {
		name string
		args args
	}{
		{
			name: "test initializing KAS config",
			args: args{
				legacyJwksURL: testServer.URL + "/kas",
				ssaJwksURL:    testServer.URL + "/ssa",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewTokenOriginConfig()
			if got == nil {
				t.Errorf("Unable to initialize JWT Origin")
			}

			// Add token origins using the new API
			got.AddConfig("kas", "authn.nvidia.com", tt.args.legacyJwksURL, TokenOriginKasLegacy, false, nil, nil)
			got.AddConfig("ssa", "ssa.nvidia.com", tt.args.ssaJwksURL, TokenOriginKasSsa, false, nil, nil)

			// Verify configurations were added correctly
			kasConfig := got.GetFirstConfigByOrigin(TokenOriginKasLegacy)
			if kasConfig == nil {
				t.Errorf("KAS config was not added correctly")
			}
			if kasConfig != nil && kasConfig.URL != tt.args.legacyJwksURL {
				t.Errorf("KAS config URL = %v, want %v", kasConfig.URL, tt.args.legacyJwksURL)
			}

			ssaConfig := got.GetFirstConfigByOrigin(TokenOriginKasSsa)
			if ssaConfig == nil {
				t.Errorf("SSA config was not added correctly")
			}
			if ssaConfig != nil && ssaConfig.URL != tt.args.ssaJwksURL {
				t.Errorf("SSA config URL = %v, want %v", ssaConfig.URL, tt.args.ssaJwksURL)
			}
		})
	}
}

func TestTokenOriginConfig_UpdateAllJWKS(t *testing.T) {
	t.Run("all issuers unavailable", func(t *testing.T) {
		testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
			res.WriteHeader(http.StatusServiceUnavailable)
		}))
		defer testServer.Close()

		config := NewTokenOriginConfig()
		config.AddConfig("keycloak", "test-issuer", testServer.URL, TokenOriginKeycloak, false, nil, nil)

		err := config.UpdateAllJWKS()
		require.EqualError(t, err, "all JWKS updates failed (1 issuers)")
		assert.NotNil(t, config.GetConfig("test-issuer"))
		assert.Nil(t, config.GetConfig("test-issuer").GetJWKS())
	})

	t.Run("partial failure keeps healthy issuer available", func(t *testing.T) {
		testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
			if req.URL.Path == "/healthy" {
				res.Header().Set("Content-Type", "application/json")
				res.WriteHeader(http.StatusOK)
				_, err := res.Write([]byte(ssaJwks))
				assert.NoError(t, err)
				return
			}
			res.WriteHeader(http.StatusServiceUnavailable)
		}))
		defer testServer.Close()

		config := NewTokenOriginConfig()
		config.AddConfig("healthy", "healthy-issuer", testServer.URL+"/healthy", TokenOriginKasSsa, false, nil, nil)
		config.AddConfig("unavailable", "unavailable-issuer", testServer.URL+"/unavailable", TokenOriginKeycloak, false, nil, nil)

		require.NoError(t, config.UpdateAllJWKS())
		assert.NotNil(t, config.GetConfig("healthy-issuer").GetJWKS())
		assert.Nil(t, config.GetConfig("unavailable-issuer").GetJWKS())
	})
}

// TestJWTOptionalKID_GoJose tests JWT validation with tokens created using the actual go-jose library
func TestJWTOptionalKID_GoJose(t *testing.T) {
	// Generate RSA key pair for signing
	privateKey, err := rsa.GenerateKey(rand.Reader, 2048)
	require.NoError(t, err, "Failed to generate RSA key")

	// Create go-jose signer with and without kid
	tests := []struct {
		name          string
		includeKid    bool
		expectedKid   string
		jwksKeyId     string
		shouldSucceed bool
		description   string
	}{
		{
			name:          "Token without kid, single key in JWKS",
			includeKid:    false,
			expectedKid:   "",
			jwksKeyId:     "test-key-1",
			shouldSucceed: true,
			description:   "Should work because there's only one key matching RS256",
		},
		{
			name:          "Token with kid, matching key in JWKS",
			includeKid:    true,
			expectedKid:   "test-key-1",
			jwksKeyId:     "test-key-1",
			shouldSucceed: true,
			description:   "Should work because kid matches JWKS key",
		},
		{
			name:          "Token with kid, non-matching key in JWKS",
			includeKid:    true,
			expectedKid:   "wrong-key-id",
			jwksKeyId:     "test-key-1",
			shouldSucceed: false,
			description:   "Should fail because kid doesn't match any JWKS key",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create JWT token using go-jose
			tokenString, err := createTokenWithGoJose(privateKey, tt.includeKid, tt.expectedKid)
			require.NoError(t, err, "Failed to create token with go-jose")

			// Create mock JWKS server with the public key
			jwksServer := createMockJWKSServer(t, privateKey.Public().(*rsa.PublicKey), tt.jwksKeyId)
			defer jwksServer.Close()

			// Create JWKS config and update keys
			jwksConfig := NewJwksConfig("test-ssa-config", jwksServer.URL, "test-issuer", TokenOriginKasSsa, false, nil, nil)
			err = jwksConfig.UpdateJWKS()
			require.NoError(t, err, "Failed to update JWKS")

			// Test token validation directly with JWKS config
			token, err := jwksConfig.ValidateToken(tokenString, jwt.MapClaims{})

			if tt.shouldSucceed {
				assert.NoError(t, err, "Token validation should succeed: %s", tt.description)
				assert.NotNil(t, token, "Token should not be nil")
				if token != nil {
					assert.True(t, token.Valid, "Token should be valid")
				}
				t.Logf("Success: %s", tt.description)
			} else {
				assert.Error(t, err, "Token validation should fail: %s", tt.description)
				t.Logf("Expected failure: %s - Error: %v", tt.description, err)
			}
		})
	}
}

// TestJWTOptionalKID_MultipleKeys tests scenarios with multiple keys in JWKS
func TestJWTOptionalKID_MultipleKeys(t *testing.T) {
	// Generate multiple RSA key pairs
	privateKey1, err := rsa.GenerateKey(rand.Reader, 2048)
	require.NoError(t, err, "Failed to generate RSA key 1")

	privateKey2, err := rsa.GenerateKey(rand.Reader, 2048)
	require.NoError(t, err, "Failed to generate RSA key 2")

	tests := []struct {
		name          string
		signingKey    *rsa.PrivateKey
		includeKid    bool
		tokenKid      string
		shouldSucceed bool
		description   string
	}{
		{
			name:          "No kid, multiple keys, first key signs",
			signingKey:    privateKey1,
			includeKid:    false,
			tokenKid:      "",
			shouldSucceed: true,
			description:   "Should use first available signing key when no kid specified",
		},
		{
			name:          "No kid, multiple keys, second key signs",
			signingKey:    privateKey2,
			includeKid:    false,
			tokenKid:      "",
			shouldSucceed: true,
			description:   "Should succeed by trying all candidate keys when no kid specified",
		},
		{
			name:          "With kid, exact match",
			signingKey:    privateKey2,
			includeKid:    true,
			tokenKid:      "key-2",
			shouldSucceed: true,
			description:   "Should work when kid matches exactly",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create JWT token using go-jose
			tokenString, err := createTokenWithGoJose(tt.signingKey, tt.includeKid, tt.tokenKid)
			require.NoError(t, err, "Failed to create token with go-jose")

			// Create mock JWKS server with multiple keys
			jwksServer := createMockJWKSServerMultipleKeys(t,
				privateKey1.Public().(*rsa.PublicKey), "key-1",
				privateKey2.Public().(*rsa.PublicKey), "key-2")
			defer jwksServer.Close()

			// Create JWKS config and update keys
			jwksConfig := NewJwksConfig("test-ssa-config", jwksServer.URL, "test-issuer", TokenOriginKasSsa, false, nil, nil)
			err = jwksConfig.UpdateJWKS()
			require.NoError(t, err, "Failed to update JWKS")

			// Test token validation directly with JWKS config
			token, err := jwksConfig.ValidateToken(tokenString, jwt.MapClaims{})

			if tt.shouldSucceed {
				assert.NoError(t, err, "Token validation should succeed: %s", tt.description)
				assert.NotNil(t, token, "Token should not be nil")
				if token != nil {
					assert.True(t, token.Valid, "Token should be valid")
				}
				t.Logf("Success: %s", tt.description)
			} else {
				assert.Error(t, err, "Token validation should fail: %s", tt.description)
				t.Logf("Expected failure: %s - Error: %v", tt.description, err)
			}
		})
	}
}

// TestJWTOptionalKID_AlgorithmMatching tests algorithm-based key selection
func TestJWTOptionalKID_AlgorithmMatching(t *testing.T) {
	// Generate keys for different algorithms
	rsaKey, err := rsa.GenerateKey(rand.Reader, 2048)
	require.NoError(t, err, "Failed to generate RSA key")

	tests := []struct {
		name           string
		algorithm      string
		jwksAlgorithms []string
		shouldSucceed  bool
		description    string
	}{
		{
			name:           "RS256 token, RS256 key available",
			algorithm:      "RS256",
			jwksAlgorithms: []string{"RS256"},
			shouldSucceed:  true,
			description:    "Should match RS256 algorithm",
		},
		{
			name:           "RS256 token, multiple algorithms available",
			algorithm:      "RS256",
			jwksAlgorithms: []string{"RS512", "RS256", "PS256"},
			shouldSucceed:  true,
			description:    "Should find RS256 among multiple algorithms",
		},
		{
			name:           "RS256 token, no matching algorithm",
			algorithm:      "RS256",
			jwksAlgorithms: []string{}, // Empty algorithms will cause JWKS to have no keys
			shouldSucceed:  false,
			description:    "Should fail when no keys are available",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create JWT token without kid
			tokenString, err := createTokenWithGoJose(rsaKey, false, "")
			require.NoError(t, err, "Failed to create token with go-jose")

			// Create mock JWKS server with specified algorithms
			jwksServer := createMockJWKSServerWithAlgorithms(t, rsaKey.Public().(*rsa.PublicKey), tt.jwksAlgorithms)
			defer jwksServer.Close()

			// Create JWKS config
			jwksConfig := NewJwksConfig("test-ssa-config", jwksServer.URL, "test-issuer", TokenOriginKasSsa, false, nil, nil)
			err = jwksConfig.UpdateJWKS()

			if tt.shouldSucceed {
				require.NoError(t, err, "Failed to update JWKS")

				// Test token validation directly with JWKS config
				token, err := jwksConfig.ValidateToken(tokenString, jwt.MapClaims{})

				assert.NoError(t, err, "Token validation should succeed: %s", tt.description)
				assert.NotNil(t, token, "Token should not be nil")
				if token != nil {
					assert.True(t, token.Valid, "Token should be valid")
				}
				t.Logf("Success: %s", tt.description)
			} else {
				// Expect JWKS update to fail for tests with no matching algorithms
				if len(tt.jwksAlgorithms) == 0 {
					require.Error(t, err, "Expected JWKS update to fail for empty algorithms")
					t.Logf("Expected failure: %s - JWKS Update Error: %v", tt.description, err)
				} else {
					require.NoError(t, err, "Failed to update JWKS")

					// Test token validation - should fail
					_, err := jwksConfig.ValidateToken(tokenString, jwt.MapClaims{})
					assert.Error(t, err, "Token validation should fail: %s", tt.description)
					t.Logf("Expected failure: %s - Token Validation Error: %v", tt.description, err)
				}
			}
		})
	}
}

// Helper function to create JWT token using actual go-jose library
func createTokenWithGoJose(privateKey *rsa.PrivateKey, includeKid bool, kid string) (string, error) {
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

	// Create go-jose signer with conditional kid header
	var signer jose.Signer
	if includeKid {
		signerOptions := &jose.SignerOptions{}
		signerOptions = signerOptions.WithHeader("kid", kid)

		signer, err = jose.NewSigner(
			jose.SigningKey{Algorithm: jose.RS256, Key: privateKey},
			signerOptions,
		)
	} else {
		// Create signer without kid header
		signer, err = jose.NewSigner(
			jose.SigningKey{Algorithm: jose.RS256, Key: privateKey},
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

// Helper function to create mock JWKS server with single key
func createMockJWKSServer(t *testing.T, publicKey *rsa.PublicKey, keyId string) *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		jwks := createJWKSResponse(publicKey, keyId, "RS256", "sig")
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(jwks))
	}))
}

// Helper function to create mock JWKS server with multiple keys
func createMockJWKSServerMultipleKeys(t *testing.T, publicKey1 *rsa.PublicKey, keyId1 string, publicKey2 *rsa.PublicKey, keyId2 string) *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// Create each key separately to avoid JSON formatting issues
		key1N := encodeBase64URLBigInt(publicKey1.N)
		key1E := encodeBase64URLBigInt(big.NewInt(int64(publicKey1.E)))
		key2N := encodeBase64URLBigInt(publicKey2.N)
		key2E := encodeBase64URLBigInt(big.NewInt(int64(publicKey2.E)))

		jwks := `{"keys":[{"kty":"RSA","kid":"` + keyId1 + `","alg":"RS256","use":"sig","n":"` + key1N + `","e":"` + key1E + `"},{"kty":"RSA","kid":"` + keyId2 + `","alg":"RS256","use":"sig","n":"` + key2N + `","e":"` + key2E + `"}]}`

		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(jwks))
	}))
}

// Helper function to create mock JWKS server with specific algorithms
func createMockJWKSServerWithAlgorithms(t *testing.T, publicKey *rsa.PublicKey, algorithms []string) *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var jwks string
		if len(algorithms) > 0 {
			// Only include the key if RS256 is in the algorithms list
			hasRS256 := false
			for _, alg := range algorithms {
				if alg == "RS256" {
					hasRS256 = true
					break
				}
			}
			if hasRS256 {
				jwks = createJWKSResponse(publicKey, "test-key", "RS256", "sig")
			} else {
				// Return empty JWKS if no matching algorithm
				jwks = `{"keys": []}`
			}
		} else {
			jwks = `{"keys": []}`
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(jwks))
	}))
}

// Helper function to create JWKS JSON response with single key
func createJWKSResponse(publicKey *rsa.PublicKey, keyId, algorithm, use string) string {
	return `{
		"keys": [{
			"kty": "RSA",
			"kid": "` + keyId + `",
			"alg": "` + algorithm + `",
			"use": "` + use + `",
			"n": "` + encodeBase64URLBigInt(publicKey.N) + `",
			"e": "` + encodeBase64URLBigInt(big.NewInt(int64(publicKey.E))) + `"
		}]
	}`
}

// Helper function to encode big.Int as base64url
func encodeBase64URLBigInt(n *big.Int) string {
	return base64.RawURLEncoding.EncodeToString(n.Bytes())
}
