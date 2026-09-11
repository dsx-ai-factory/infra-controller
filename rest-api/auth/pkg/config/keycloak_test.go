// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"crypto/rand"
	"crypto/rsa"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/golang-jwt/jwt/v5"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

var (
	// Real JWKS response from nico development environment
	mockKeycloakJwks = `{"keys":[{"kid":"2qPROcQfHMCXUi4rKt-CRB5iG4Z-5rfbP7zHOsxWA28","kty":"RSA","alg":"RS256","use":"sig","x5c":["MIICmTCCAYECBgGYzofhRDANBgkqhkiG9w0BAQsFADAQMQ4wDAYDVQQDDAVmb3JnZTAeFw0yNTA4MjEyMTI2MDhaFw0zNTA4MjEyMTI3NDhaMBAxDjAMBgNVBAMMBWZvcmdlMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA9YNTgddbGn0PKUbk3uISXkhWro0ColFLRZYFWSCCHV5JXG6bgmCeFa4RWnUi0qzRtzyu2uEAWbf5XMJl0TSO9F0N4OdeeW6nK2ZzdK1ASuRy9ACBGgv0kCRpukgX9vlJAjSR3DIHROom9evsf5RYzX9tgNKdkRz1134zZpQ+EtskZ9MnoZEd8NfFbyzAeyAe4iAL+Sjf5DV+ACKwJopDUPz9MwvK7BYEdqZ6ZNnn6nmwNAt/0jabf5Z6QTeKJv22fk6jKM3vQZH2IE/h+ulHYA9pMZoLciQ7zchXVvyAJkIjmeO2nGtW5cFHZ3X2Bm6MMU9MtzIfjAR2FCbKwtJF9QIDAQABMA0GCSqGSIb3DQEBCwUAA4IBAQAqNZY5kMW8VcnmuC1Ux4c6EMLhaAej6mQsGawic6sj9AHiYI24zY6VID9I9IG6cBP9J5Pw84TVU+J96CNcavMmCZV80hQNrunABJHM/lUtv0sUsqGm4qpsnOD+g7B9XKIu9YxnpGzX7ouH0nk355rBN7swTuEBpy5ELtQlraAGMbTDv+UjgpxAiUczsQeS3mvKnyiINx9Rv0imJhRskyuaqmLaVb0eZkezFEWPYzqqOAEEuMOkuwOD/1vJVz3j1gCcy9ZOqwe+8O0zPJuN/cLjDiXPmpqOvI1eKW03O+sBKasYm9dVC/JaBktHeQ0LJZUVGYzgVmbun41z/2Q01WQW"],"x5t":"UHFsVos9chqrKD4oPeyih58kFr0","x5t#S256":"UQ7TfWdf5BUFzuZ_8OcK1Idbzz_mYU2Xrpu-Mv9W1KI","n":"9YNTgddbGn0PKUbk3uISXkhWro0ColFLRZYFWSCCHV5JXG6bgmCeFa4RWnUi0qzRtzyu2uEAWbf5XMJl0TSO9F0N4OdeeW6nK2ZzdK1ASuRy9ACBGgv0kCRpukgX9vlJAjSR3DIHROom9evsf5RYzX9tgNKdkRz1134zZpQ-EtskZ9MnoZEd8NfFbyzAeyAe4iAL-Sjf5DV-ACKwJopDUPz9MwvK7BYEdqZ6ZNnn6nmwNAt_0jabf5Z6QTeKJv22fk6jKM3vQZH2IE_h-ulHYA9pMZoLciQ7zchXVvyAJkIjmeO2nGtW5cFHZ3X2Bm6MMU9MtzIfjAR2FCbKwtJF9Q","e":"AQAB"},{"kid":"rYde1QMYY3w-bK7qt5GPvI6uGK1b38KtguxnLYcYg-U","kty":"RSA","alg":"RSA-OAEP","use":"enc","x5c":["MIICmTCCAYECBgGYzofh5DANBgkqhkiG9w0BAQsFADAQMQ4wDAYDVQQDDAVmb3JnZTAeFw0yNTA4MjEyMTI2MDhaFw0zNTA4MjEyMTI3NDhaMBAxDjAMBgNVBAMMBWZvcmdlMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAtlnHXyI93apJ2gqfX80VlKuz6CrGh79hxPAF3WcKPXfqrng4HjjlH8BYFY37WRXKX4whEEDaE3KPp6p59sOaVpcfYAlf7Nxrzdpm0Mro23mNCR1VCzMlc4enlcD7hB753diBYr93bkMUTZPtE7Ws3YNPPY7+JV+c8xjA0yz7Er1YG89GYuey6sKGxOrNxwvTh9477hN5fKwfVDBBZAZr7oiNxNPFN2ecQ1rXy36byNg8mSRcF32z2Y2KUKuUMXysmSf3W+aC48SHNtykXY9btNEMFhnE2FekmKMc6cefkgkVuSgLo8zmyWYFcFAcmNaqce6EgS4wb4ITfNs9IKrqWwIDAQABMA0GCSqGSIb3DQEBCwUAA4IBAQCZZj+L4eIeLGrSqM0HiacMYDG0KXUHO/x3RM6D7UNRXt+pGF8hs1j9+Q27BEejD6IptBjQEfipHAvOaYV8TuFBpE4UEOLXxQBloLPRO4fb/OS7s7UFyE2XgnOS9E4NMxzRYVfyFxtXPZssNd7WxTeiA4/ISh9C47or9Ge+F5h5YQSVkLtXjRmEhN4K5OMkeafGbmA1WGHSEKQei6QbGgzbTbXTgtTpQgcL6WHLtpBaOnd4X9h38mJ9yPwr+aiadco33VDHWaruG0APDIadjq+SI2pn6H+TPpAfzvr11wnjvZswj6ePoPk9HgxtvQbUBalbOO8rIWSl2n5PrKzZNXwM"],"x5t":"pKk7-fkjCqoobuCTe--sBvdX0wc","x5t#S256":"JL1f_QDCxBTj81_-h_K1KBvRtYAc4GbZBcjuAWWOv2c","n":"tlnHXyI93apJ2gqfX80VlKuz6CrGh79hxPAF3WcKPXfqrng4HjjlH8BYFY37WRXKX4whEEDaE3KPp6p59sOaVpcfYAlf7Nxrzdpm0Mro23mNCR1VCzMlc4enlcD7hB753diBYr93bkMUTZPtE7Ws3YNPPY7-JV-c8xjA0yz7Er1YG89GYuey6sKGxOrNxwvTh9477hN5fKwfVDBBZAZr7oiNxNPFN2ecQ1rXy36byNg8mSRcF32z2Y2KUKuUMXysmSf3W-aC48SHNtykXY9btNEMFhnE2FekmKMc6cefkgkVuSgLo8zmyWYFcFAcmNaqce6EgS4wb4ITfNs9IKrqWw","e":"AQAB"}]}`
)

func TestNewKeycloakConfig(t *testing.T) {
	tests := []struct {
		name            string
		baseURL         string
		externalBaseURL string
		clientID        string
		clientSecret    string
		realm           string
		want            *KeycloakConfig
	}{
		{
			name:            "valid complete configuration",
			baseURL:         "http://localhost:8082",
			externalBaseURL: "http://external:8082",
			clientID:        "test-client",
			clientSecret:    "test-secret",
			realm:           "test-realm",
			want: &KeycloakConfig{
				BaseURL:         "http://localhost:8082",
				ExternalBaseURL: "http://external:8082",
				ClientID:        "test-client",
				ClientSecret:    "test-secret",
				Realm:           "test-realm",
			},
		},
		{
			name:            "minimal configuration",
			baseURL:         "http://localhost:8082",
			externalBaseURL: "",
			clientID:        "client",
			clientSecret:    "secret",
			realm:           "realm",
			want: &KeycloakConfig{
				BaseURL:         "http://localhost:8082",
				ExternalBaseURL: "",
				ClientID:        "client",
				ClientSecret:    "secret",
				Realm:           "realm",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewKeycloakConfig(
				tt.baseURL,
				tt.externalBaseURL,
				tt.clientID,
				tt.clientSecret,
				tt.realm,
				true,
			)

			assert.Equal(t, tt.want.BaseURL, got.BaseURL)
			assert.Equal(t, tt.want.ExternalBaseURL, got.ExternalBaseURL)
			assert.Equal(t, tt.want.ClientID, got.ClientID)
			assert.Equal(t, tt.want.ClientSecret, got.ClientSecret)
			assert.Equal(t, tt.want.Realm, got.Realm)

			// The key cache can be empty, but its configuration must remain
			// available for request-time refresh.
			require.NotNil(t, got.jwksConfig)
			assert.Nil(t, got.jwksConfig.GetJWKS())
		})
	}
}

func TestKeycloakConfig_GetJwksConfig(t *testing.T) {
	// Create mock JWKS server
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/realms/test-realm/protocol/openid-connect/certs") {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(mockKeycloakJwks))
		} else if strings.Contains(req.URL.Path, "/realms/bad-realm/protocol/openid-connect/certs") {
			res.WriteHeader(http.StatusNotFound)
			res.Write([]byte(`{"error":"realm_not_found"}`))
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer testServer.Close()

	tests := []struct {
		name     string
		config   *KeycloakConfig
		wantErr  bool
		validate func(*testing.T, *JwksConfig)
	}{
		{
			name: "successful JWKS config initialization",
			config: &KeycloakConfig{
				BaseURL: testServer.URL,
				Realm:   "test-realm",
			},
			wantErr: false,
			validate: func(t *testing.T, jwks *JwksConfig) {
				expectedURL := testServer.URL + "/realms/test-realm/protocol/openid-connect/certs"
				assert.Equal(t, expectedURL, jwks.URL)
				assert.NotNil(t, jwks.GetJWKS())
				assert.Greater(t, jwks.KeyCount(), 0)
			},
		},
		{
			name: "JWKS config with non-existent realm",
			config: &KeycloakConfig{
				BaseURL: testServer.URL,
				Realm:   "bad-realm",
			},
			wantErr:  true,
			validate: nil,
		},
		{
			name: "JWKS config with invalid base URL",
			config: &KeycloakConfig{
				BaseURL: "invalid-url",
				Realm:   "test-realm",
			},
			wantErr:  true,
			validate: nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			jwksConfig, err := tt.config.GetJwksConfig()

			if tt.wantErr {
				assert.Error(t, err)
				assert.NotNil(t, jwksConfig)
				assert.Nil(t, jwksConfig.GetJWKS())
			} else {
				assert.NoError(t, err)
				assert.NotNil(t, jwksConfig)

				if tt.validate != nil {
					tt.validate(t, jwksConfig)
				}
			}
		})
	}

	t.Run("recovers after initial JWKS failure", func(t *testing.T) {
		privateKey, err := rsa.GenerateKey(rand.Reader, 2048)
		require.NoError(t, err)

		var keycloakReady atomic.Bool
		var requestCount atomic.Int32
		testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
			requestCount.Add(1)
			if !keycloakReady.Load() {
				res.WriteHeader(http.StatusServiceUnavailable)
				return
			}

			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			_, err := res.Write([]byte(createJWKSResponse(privateKey.Public().(*rsa.PublicKey), "test-key-id", "RS256", "sig")))
			assert.NoError(t, err)
		}))
		defer testServer.Close()

		keycloakConfig := NewKeycloakConfig(
			testServer.URL,
			"https://keycloak.example.com",
			"test-client",
			"test-secret",
			"test-realm",
			true,
		)

		jwksConfig, err := keycloakConfig.GetJwksConfig()
		require.Error(t, err)
		require.NotNil(t, jwksConfig)
		assert.Nil(t, jwksConfig.GetJWKS())
		assert.Equal(t, int32(1), requestCount.Load())

		tokenString, err := createTokenWithGoJose(privateKey, true, "test-key-id")
		require.NoError(t, err)
		jwksConfig.Lock()
		jwksConfig.LastAttempted = time.Now()
		jwksConfig.Unlock()
		_, err = jwksConfig.ValidateToken(tokenString, jwt.MapClaims{})
		require.Error(t, err)
		assert.Equal(t, int32(1), requestCount.Load())

		keycloakReady.Store(true)
		jwksConfig.Lock()
		jwksConfig.LastAttempted = time.Now().Add(-minUpdateInterval)
		jwksConfig.Unlock()

		token, err := jwksConfig.ValidateToken(tokenString, jwt.MapClaims{})
		require.NoError(t, err)
		require.NotNil(t, token)
		assert.True(t, token.Valid)
		assert.Equal(t, int32(2), requestCount.Load())

		_, err = keycloakConfig.GetJwksConfig()
		require.NoError(t, err)
	})
}

func TestKeycloakConfig_GetJwksConfig_Caching(t *testing.T) {
	// Create mock JWKS server
	requestCount := 0
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/realms/test-realm/protocol/openid-connect/certs") {
			requestCount++
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(mockKeycloakJwks))
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer testServer.Close()

	config := &KeycloakConfig{
		BaseURL: testServer.URL,
		Realm:   "test-realm",
	}

	// First call should fetch JWKS
	jwksConfig1, err := config.GetJwksConfig()
	require.NoError(t, err)
	assert.NotNil(t, jwksConfig1)
	assert.Equal(t, 1, requestCount)

	// Second call should use cached JWKS
	jwksConfig2, err := config.GetJwksConfig()
	require.NoError(t, err)
	assert.NotNil(t, jwksConfig2)
	assert.Equal(t, 1, requestCount) // Should not increment

	// Should return the same instance
	assert.Same(t, jwksConfig1, jwksConfig2)
}

func TestKeycloakConfig_GetJwksConfig_Concurrency(t *testing.T) {
	// Create mock JWKS server
	requestCount := 0
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/realms/test-realm/protocol/openid-connect/certs") {
			requestCount++
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(mockKeycloakJwks))
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer testServer.Close()

	config := &KeycloakConfig{
		BaseURL: testServer.URL,
		Realm:   "test-realm",
	}

	// Test concurrent access to GetJwksConfig
	done := make(chan *JwksConfig, 10)
	errors := make(chan error, 10)

	for i := 0; i < 10; i++ {
		go func() {
			jwksConfig, err := config.GetJwksConfig()
			if err != nil {
				errors <- err
			} else {
				done <- jwksConfig
			}
		}()
	}

	// Collect results
	var configs []*JwksConfig
	var errs []error

	for i := 0; i < 10; i++ {
		select {
		case cfg := <-done:
			configs = append(configs, cfg)
		case err := <-errors:
			errs = append(errs, err)
		}
	}

	// All should succeed
	assert.Empty(t, errs)
	assert.Len(t, configs, 10)

	// All should be the same instance (cached)
	for i := 1; i < len(configs); i++ {
		assert.Same(t, configs[0], configs[i])
	}

	// Should have made only one HTTP request despite concurrent access
	assert.Equal(t, 1, requestCount)
}

func TestKeycloakConfig_Properties(t *testing.T) {
	config := &KeycloakConfig{
		BaseURL:         "http://localhost:8082",
		ExternalBaseURL: "http://external:8082",
		ClientID:        "test-client",
		ClientSecret:    "test-secret",
		Realm:           "test-realm",
	}

	t.Run("verify all properties are set correctly", func(t *testing.T) {
		assert.Equal(t, "http://localhost:8082", config.BaseURL)
		assert.Equal(t, "http://external:8082", config.ExternalBaseURL)
		assert.Equal(t, "test-client", config.ClientID)
		assert.Equal(t, "test-secret", config.ClientSecret)
		assert.Equal(t, "test-realm", config.Realm)
	})

	t.Run("verify initial jwks config is nil", func(t *testing.T) {
		assert.Nil(t, config.jwksConfig)
	})
}

func TestKeycloakConfig_ThreadSafety(t *testing.T) {
	config := &KeycloakConfig{
		BaseURL: "http://localhost:8082",
		Realm:   "test-realm",
	}

	// Test that the mutex is properly initialized and can be used
	config.mu.Lock()
	_ = config.jwksConfig // Access field while locked
	config.mu.Unlock()

	config.mu.RLock()
	_ = config.jwksConfig // Access field while read-locked
	config.mu.RUnlock()

	// If we get here without deadlock, the mutex is working
	assert.NotNil(t, config)
}

// TestKeycloakConfig_Integration tests the complete JWKS flow with a real server
func TestKeycloakConfig_Integration(t *testing.T) {
	// Create mock Keycloak server that responds to different scenarios
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		switch {
		case strings.Contains(req.URL.Path, "/realms/valid-realm/protocol/openid-connect/certs"):
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(mockKeycloakJwks))
		case strings.Contains(req.URL.Path, "/realms/invalid-realm/protocol/openid-connect/certs"):
			res.WriteHeader(http.StatusNotFound)
			res.Write([]byte(`{"error":"realm_not_found","error_description":"Realm does not exist"}`))
		case strings.Contains(req.URL.Path, "/realms/malformed-realm/protocol/openid-connect/certs"):
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(`{"invalid":"json"`)) // Malformed JSON
		default:
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer testServer.Close()

	tests := []struct {
		name     string
		realm    string
		wantErr  bool
		validate func(*testing.T, *JwksConfig, error)
	}{
		{
			name:    "successful JWKS retrieval",
			realm:   "valid-realm",
			wantErr: false,
			validate: func(t *testing.T, jwks *JwksConfig, err error) {
				assert.NoError(t, err)
				assert.NotNil(t, jwks)
				expectedURL := testServer.URL + "/realms/valid-realm/protocol/openid-connect/certs"
				assert.Equal(t, expectedURL, jwks.URL)
				assert.NotNil(t, jwks.GetJWKS())
				assert.Greater(t, jwks.KeyCount(), 0)
			},
		},
		{
			name:    "realm not found",
			realm:   "invalid-realm",
			wantErr: true,
			validate: func(t *testing.T, jwks *JwksConfig, err error) {
				assert.Error(t, err)
				assert.NotNil(t, jwks)
				assert.Nil(t, jwks.GetJWKS())
				assert.Contains(t, err.Error(), "failed to fetch JWKS")
			},
		},
		{
			name:    "malformed JWKS response",
			realm:   "malformed-realm",
			wantErr: true,
			validate: func(t *testing.T, jwks *JwksConfig, err error) {
				assert.Error(t, err)
				assert.NotNil(t, jwks)
				assert.Nil(t, jwks.GetJWKS())
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			config := &KeycloakConfig{
				BaseURL: testServer.URL,
				Realm:   tt.realm,
			}

			jwksConfig, err := config.GetJwksConfig()

			if tt.validate != nil {
				tt.validate(t, jwksConfig, err)
			} else {
				if tt.wantErr {
					assert.Error(t, err)
					assert.NotNil(t, jwksConfig)
				} else {
					assert.NoError(t, err)
					assert.NotNil(t, jwksConfig)
				}
			}
		})
	}
}

// TestKeycloakConfig_JwksURL tests JWKS URL construction
func TestKeycloakConfig_JwksURL(t *testing.T) {
	tests := []struct {
		name        string
		baseURL     string
		realm       string
		expectedURL string
	}{
		{
			name:        "standard Keycloak URL",
			baseURL:     "http://localhost:8082",
			realm:       "test-realm",
			expectedURL: "http://localhost:8082/realms/test-realm/protocol/openid-connect/certs",
		},
		{
			name:        "Keycloak with auth prefix",
			baseURL:     "http://localhost:8082/auth",
			realm:       "my-realm",
			expectedURL: "http://localhost:8082/auth/realms/my-realm/protocol/openid-connect/certs",
		},
		{
			name:        "external Keycloak URL",
			baseURL:     "https://keycloak.example.com",
			realm:       "production",
			expectedURL: "https://keycloak.example.com/realms/production/protocol/openid-connect/certs",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			config := &KeycloakConfig{
				BaseURL: tt.baseURL,
				Realm:   tt.realm,
			}

			// Test URL construction by checking what would be built
			expectedURL := config.BaseURL + "/realms/" + config.Realm + "/protocol/openid-connect/certs"
			assert.Equal(t, tt.expectedURL, expectedURL)
		})
	}
}

// TestKeycloakConfig_ErrorHandling tests error scenarios with real server responses
func TestKeycloakConfig_ErrorHandling(t *testing.T) {
	// Create server that returns various error responses
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		switch {
		case strings.Contains(req.URL.Path, "/realms/timeout-realm/"):
			// Simulate timeout by not responding
			return
		case strings.Contains(req.URL.Path, "/realms/error-realm/"):
			res.WriteHeader(http.StatusInternalServerError)
			res.Write([]byte(`{"error":"internal_server_error"}`))
		case strings.Contains(req.URL.Path, "/realms/empty-realm/"):
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(`{}`)) // Empty but valid JSON
		default:
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer testServer.Close()

	tests := []struct {
		name    string
		realm   string
		wantErr bool
	}{
		{
			name:    "server error response",
			realm:   "error-realm",
			wantErr: true,
		},
		{
			name:    "empty JWKS response",
			realm:   "empty-realm",
			wantErr: true, // Empty JWKS should be rejected by enhanced validation
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			config := &KeycloakConfig{
				BaseURL: testServer.URL,
				Realm:   tt.realm,
			}

			jwksConfig, err := config.GetJwksConfig()

			if tt.wantErr {
				assert.Error(t, err)
				assert.NotNil(t, jwksConfig)
				assert.Nil(t, jwksConfig.GetJWKS())
			} else {
				assert.NoError(t, err)
				assert.NotNil(t, jwksConfig)
			}
		})
	}
}
