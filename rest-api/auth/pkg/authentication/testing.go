// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authentication

import (
	"crypto/rand"
	"crypto/rsa"
	"encoding/base64"
	"fmt"
	"math/big"
	"net/http"
	"net/http/httptest"
	"strings"
	"time"

	"github.com/golang-jwt/jwt/v5"
)

// TestMockResponses contains all mock response data
type TestMockResponses struct {
	AdminLogin string
	IDPs       string
	Token      string
	UserInfo   string
	JWKS       string
}

// GetDefaultMockResponses provides standard mock responses for tests
// This generates JWKS dynamically based on the consistent test RSA key
func GetDefaultMockResponses() TestMockResponses {
	// Get the consistent test key from config package
	testKey := getConsistentTestRSAKey()

	// Generate JWKS from the public key
	jwks := generateJWKSFromRSAKey(&testKey.PublicKey, "test-key-id")

	return TestMockResponses{
		AdminLogin: `{"access_token":"admin-access-token","expires_in":300,"refresh_expires_in":1800,"refresh_token":"admin-refresh-token","token_type":"Bearer","not-before-policy":0,"session_state":"test-session-state","scope":"profile email"}`,
		IDPs:       `[{"alias":"testorg-idp","displayName":"TestOrg OIDC","providerId":"oidc","enabled":true,"config":{"kc.org.domain":"testorg.com","clientId":"testorg-client","clientSecret":"testorg-secret"}}]`,
		Token:      `{"access_token":"test-access-token","expires_in":3600,"refresh_expires_in":1800,"refresh_token":"test-refresh-token","token_type":"Bearer"}`,
		UserInfo:   `{"sub":"user-123","email":"john.doe@testorg.com","preferred_username":"john.doe","given_name":"John","family_name":"Doe"}`,
		JWKS:       jwks,
	}
}

// DefaultMockResponses provides standard mock responses for tests (backward compatibility)
var DefaultMockResponses = GetDefaultMockResponses()

// MockKeycloakServerConfig configures the mock server behavior
type MockKeycloakServerConfig struct {
	Responses        TestMockResponses
	ValidCredentials map[string]string // username -> password
	ValidTokens      map[string]bool   // token -> valid
	ValidCodes       map[string]bool   // code -> valid
}

// DefaultMockServerConfig provides a standard configuration
func DefaultMockServerConfig() MockKeycloakServerConfig {
	return MockKeycloakServerConfig{
		Responses: GetDefaultMockResponses(), // Use dynamic responses
		ValidCredentials: map[string]string{
			"admin": "admin-password",
		},
		ValidTokens: map[string]bool{
			"valid-access-token":   true,
			"admin-access-token":   true,
			"service-access-token": true,
		},
		ValidCodes: map[string]bool{
			"valid-auth-code": true,
		},
	}
}

// CreateMockKeycloakServer creates a consolidated mock Keycloak server
func CreateMockKeycloakServer(config MockKeycloakServerConfig) *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		switch {
		case strings.Contains(req.URL.Path, "/protocol/openid-connect/token") && req.Method == "POST":
			handleTokenEndpoint(res, req, config)
		case strings.Contains(req.URL.Path, "/admin/realms/") && strings.Contains(req.URL.Path, "/identity-provider/instances"):
			handleIDPEndpoint(res, req, config)
		case strings.Contains(req.URL.Path, "/protocol/openid-connect/userinfo"):
			handleUserInfoEndpoint(res, req, config)
		case strings.Contains(req.URL.Path, "/protocol/openid-connect/logout"):
			handleLogoutEndpoint(res, req, config)
		case strings.Contains(req.URL.Path, "/protocol/openid-connect/certs"):
			handleJWKSEndpoint(res, req, config)
		default:
			res.WriteHeader(http.StatusNotFound)
		}
	}))
}

func handleTokenEndpoint(res http.ResponseWriter, req *http.Request, config MockKeycloakServerConfig) {
	err := req.ParseForm()
	if err != nil {
		res.WriteHeader(http.StatusBadRequest)
		return
	}

	grantType := req.Form.Get("grant_type")
	clientID := req.Form.Get("client_id")
	clientSecret := req.Form.Get("client_secret")

	// Check for Basic Auth if form credentials are empty
	if clientID == "" || clientSecret == "" {
		username, password, ok := req.BasicAuth()
		if ok {
			clientID = username
			clientSecret = password
		}
	}

	switch grantType {
	case "password":
		username := req.Form.Get("username")
		password := req.Form.Get("password")
		if expectedPassword, exists := config.ValidCredentials[username]; exists && expectedPassword == password {
			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			// Mock response writes are best-effort if the client closes the connection.
			_, _ = res.Write([]byte(config.Responses.AdminLogin))
		} else {
			res.WriteHeader(http.StatusUnauthorized)
		}
	case "authorization_code":
		code := req.Form.Get("code")
		if config.ValidCodes[code] {
			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			_, _ = res.Write([]byte(config.Responses.Token))
		} else {
			res.WriteHeader(http.StatusBadRequest)
		}
	case "refresh_token":
		refreshToken := req.Form.Get("refresh_token")
		if refreshToken == "valid-refresh-token" {
			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			_, _ = res.Write([]byte(`{"access_token":"new-access-token","expires_in":3600,"refresh_expires_in":1800,"refresh_token":"new-refresh-token","token_type":"Bearer"}`))
		} else {
			res.WriteHeader(http.StatusBadRequest)
		}
	case "client_credentials":
		if (clientID == "service-client" && clientSecret == "service-secret") ||
			(clientID == "test-client" && clientSecret == "test-secret") {
			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			_, _ = res.Write([]byte(`{"access_token":"service-access-token","expires_in":3600,"token_type":"Bearer"}`))
		} else {
			res.WriteHeader(http.StatusUnauthorized)
		}
	default:
		res.WriteHeader(http.StatusBadRequest)
	}
}

func handleIDPEndpoint(res http.ResponseWriter, req *http.Request, config MockKeycloakServerConfig) {
	authHeader := req.Header.Get("Authorization")
	token := strings.TrimPrefix(authHeader, "Bearer ")

	// Check if the token is valid for admin operations
	if token == "admin-access-token" || config.ValidTokens[token] {
		res.Header().Set("Content-Type", "application/json")
		res.WriteHeader(http.StatusOK)
		_, _ = res.Write([]byte(config.Responses.IDPs))
	} else {
		res.WriteHeader(http.StatusUnauthorized)
	}
}

func handleUserInfoEndpoint(res http.ResponseWriter, req *http.Request, config MockKeycloakServerConfig) {
	authHeader := req.Header.Get("Authorization")
	token := strings.TrimPrefix(authHeader, "Bearer ")
	if config.ValidTokens[token] {
		res.Header().Set("Content-Type", "application/json")
		res.WriteHeader(http.StatusOK)
		_, _ = res.Write([]byte(config.Responses.UserInfo))
	} else {
		res.WriteHeader(http.StatusUnauthorized)
	}
}

func handleLogoutEndpoint(res http.ResponseWriter, req *http.Request, config MockKeycloakServerConfig) {
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

func handleJWKSEndpoint(res http.ResponseWriter, req *http.Request, config MockKeycloakServerConfig) {
	res.Header().Set("Content-Type", "application/json")
	res.WriteHeader(http.StatusOK)
	_, _ = res.Write([]byte(config.Responses.JWKS))
}

// Local reference to config package functions for backward compatibility
func getConsistentTestRSAKey() *rsa.PrivateKey {
	// Use config package's test key functionality
	return getGlobalTestRSAKey()
}

func generateJWKSFromRSAKey(publicKey *rsa.PublicKey, keyID string) string {
	// Use config package's JWKS generation
	return createJWKSFromRSAKey(publicKey, keyID)
}

// Global test RSA key - generated once and reused across tests
var globalTestRSAKey *rsa.PrivateKey

func getGlobalTestRSAKey() *rsa.PrivateKey {
	if globalTestRSAKey == nil {
		// Generate once and reuse
		key, err := rsa.GenerateKey(rand.Reader, 2048)
		if err != nil {
			panic("Failed to generate test RSA key: " + err.Error())
		}
		globalTestRSAKey = key
	}
	return globalTestRSAKey
}

func createJWKSFromRSAKey(publicKey *rsa.PublicKey, keyID string) string {
	// Convert RSA modulus (N) to base64url
	nBytes := publicKey.N.Bytes()
	n := base64.RawURLEncoding.EncodeToString(nBytes)

	// Convert RSA exponent (E) to base64url
	eBytes := big.NewInt(int64(publicKey.E)).Bytes()
	e := base64.RawURLEncoding.EncodeToString(eBytes)

	// Create JWKS JSON
	jwks := fmt.Sprintf(`{
  "keys": [
    {
      "kty": "RSA",
      "use": "sig",
      "kid": "%s",
      "alg": "RS256",
      "n": "%s",
      "e": "%s"
    }
  ]
}`, keyID, n, e)

	return jwks
}

// generateTestJWT creates a JWT token with the given claims and key
func generateTestJWT(claims jwt.Claims, key *rsa.PrivateKey) (string, error) {
	token := jwt.NewWithClaims(jwt.SigningMethodRS256, claims)
	token.Header["kid"] = "test-key-id"
	return token.SignedString(key)
}

// generateExpiredTestJWT generates an expired JWT for testing
func generateExpiredTestJWT(claims jwt.MapClaims) (string, error) {
	claims["exp"] = time.Now().Add(-time.Hour).Unix() // Expired 1 hour ago
	claims["iat"] = time.Now().Add(-2 * time.Hour).Unix()
	return generateTestJWT(claims, getConsistentTestRSAKey())
}

// generateFutureTestJWT generates a JWT that's not yet valid
func generateFutureTestJWT(claims jwt.MapClaims) (string, error) {
	claims["exp"] = time.Now().Add(2 * time.Hour).Unix()
	claims["iat"] = time.Now().Add(time.Hour).Unix() // Valid in 1 hour
	return generateTestJWT(claims, getConsistentTestRSAKey())
}

// generateMalformedJWT generates a malformed JWT for testing
func generateMalformedJWT() string {
	return "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.malformed.signature"
}

// generateJWTWithWrongSignature generates a JWT with wrong signature
func generateJWTWithWrongSignature(claims jwt.MapClaims) (string, error) {
	// Generate with different key than what will be used for validation
	wrongKey, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		return "", err
	}
	return generateTestJWT(claims, wrongKey)
}
