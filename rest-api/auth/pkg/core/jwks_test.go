// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package core

import (
	"crypto/ecdsa"
	"crypto/rsa"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	cutil "github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/util"
	"github.com/stretchr/testify/assert"
)

var (
	// Sample JWKS from real-world scenarios
	legacyKasJwks = `{"keys":[{"kty":"RSA","e":"AQAB","use":"sig","kid":"B3D2:ZQHB:M5YX:NFBJ:DFI4:U4WX:PH5E:4JXH:CNAY:WYTJ:AZWC:RALK","n":"qcqYV-iYUV6JNcMh58qLlMt8d_6AzkJcqlR77hUzR-fismhwoerT2K9vIBOno30mjKsgJjoT4zhPA8q28Sqq_AMWh7wqoBr99O75YdUawjfcngvHKCvfihN2E1Z4f-C8ihtn8T6rh9VcldLDaEhUlCIisRBTY3lnw4recPKE-cC0ejgFeOnV5Ds5a_xb1sP9Dhwv_hqIR_1Khh_H6M6WfF3Tv3eAgMQWycjCQkAY47qwXi9DCkAOhJJwlP0djsHPYKfykMKe5MUfnbPE-bCYg7rQlZfdzd58zL2G9VUyOLzZtFhGwPCA6oRyqlKTKO1dN0_wjMXa_86L0GswW-etl0HRL1KlP8ctF1m99xQ3M5leE8JOeio0eUPJNLgssClxHEW75JSXYB6T8YJek41FjQttW2sZpw1L-iQYLWVA5bIx7QEqcu85EmQQik4mvq_azX53Mug6_5tJPitdox_LQf38RIANa5zhPYcwqObjTr8W0rxMjXFN0bRrZ5f_RaXqbSdh5vVWmdzsZu0xu0otujz50ZlR5rf0W5leTs1xTLwpHh1CC2jhThwcOFkXT46zqWaKE7rsik3bp79yKHA9wkqzQOK4TE_DGp8aPrfa_8CAR1iVkbpW4diHgV-XuHLhFFjQco3I6SzPt4Ael_JoldaH2bINKvPaJXKCi_Bm9L8"}]}`
	ssaJwks       = `{"keys":[{"kty":"EC","use":"sig","crv":"P-256","kid":"2c58e180-149a-4818-9bfc-5f2a6b6dbd8a","x":"d4Sa5NYfomfkYkSdQEUrTKHXEET2dNhyQVnEViA97L0","y":"dQTndo4VhAy1G3i0Z9V6tEq7Ii2ey59pAM-GFoaI5M8","alg":"ES256"}]}`
	keycloakJwks  = `{"keys":[{"kid":"2qPROcQfHMCXUi4rKt-CRB5iG4Z-5rfbP7zHOsxWA28","kty":"RSA","alg":"RS256","use":"sig","n":"9YNTgddbGn0PKUbk3uISXkhWro0ColFLRZYFWSCCHV5JXG6bgmCeFa4RWnUi0qzRtzyu2uEAWbf5XMJl0TSO9F0N4OdeeW6nK2ZzdK1ASuRy9ACBGgv0kCRpukgX9vlJAjSR3DIHROom9evsf5RYzX9tgNKdkRz1134zZpQ-EtskZ9MnoZEd8NfFbyzAeyAe4iAL-Sjf5DV-ACKwJopDUPz9MwvK7BYEdqZ6ZNnn6nmwNAt_0jabf5Z6QTeKJv22fk6jKM3vQZH2IE_h-ulHYA9pMZoLciQ7zchXVvyAJkIjmeO2nGtW5cFHZ3X2Bm6MMU9MtzIfjAR2FCbKwtJF9Q","e":"AQAB"},{"kid":"rYde1QMYY3w-bK7qt5GPvI6uGK1b38KtguxnLYcYg-U","kty":"RSA","alg":"RSA-OAEP","use":"enc","n":"tlnHXyI93apJ2gqfX80VlKuz6CrGh79hxPAF3WcKPXfqrng4HjjlH8BYFY37WRXKX4whEEDaE3KPp6p59sOaVpcfYAlf7Nxrzdpm0Mro23mNCR1VCzMlc4enlcD7hB753diBYr93bkMUTZPtE7Ws3YNPPY7-JV-c8xjA0yz7Er1YG89GYuey6sKGxOrNxwvTh9477hN5fKwfVDBBZAZr7oiNxNPFN2ecQ1rXy36byNg8mSRcF32z2Y2KUKuUMXysmSf3W-aC48SHNtykXY9btNEMFhnE2FekmKMc6cefkgkVuSgLo8zmyWYFcFAcmNaqce6EgS4wb4ITfNs9IKrqWw","e":"AQAB"}]}`
)

func TestJWKS_GetKIDPublicKeyMap(t *testing.T) {
	tests := []struct {
		name         string
		jwksJSON     string
		expectedKids []string
		expectedRSA  bool
		expectedEC   bool
		expectedAlgs []string
	}{
		{
			name:         "RSA key from legacy KAS JWKS",
			jwksJSON:     legacyKasJwks,
			expectedKids: []string{"B3D2:ZQHB:M5YX:NFBJ:DFI4:U4WX:PH5E:4JXH:CNAY:WYTJ:AZWC:RALK"},
			expectedRSA:  true,
			expectedEC:   false,
			expectedAlgs: []string{}, // Legacy KAS JWKS has no explicit algorithm declarations
		},
		{
			name:         "ECDSA key from SSA JWKS",
			jwksJSON:     ssaJwks,
			expectedKids: []string{"2c58e180-149a-4818-9bfc-5f2a6b6dbd8a"},
			expectedRSA:  false,
			expectedEC:   true,
			expectedAlgs: []string{"ES256"}, // Only ES256 is explicitly declared
		},
		{
			name:         "Real JWKS with RSA signing and encryption keys",
			jwksJSON:     keycloakJwks,
			expectedKids: []string{"2qPROcQfHMCXUi4rKt-CRB5iG4Z-5rfbP7zHOsxWA28", "rYde1QMYY3w-bK7qt5GPvI6uGK1b38KtguxnLYcYg-U"},
			expectedRSA:  true,
			expectedEC:   false,
			expectedAlgs: []string{"RS256", "RSA-OAEP"}, // Explicitly declared algorithms
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create a test server to serve the JWKS
			testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
				res.WriteHeader(http.StatusOK)
				res.Header().Set("Content-Type", "application/json")
				res.Write([]byte(tt.jwksJSON))
			}))
			defer testServer.Close()

			// Fetch JWKS using our function
			jwks, err := NewJWKSFromURL(testServer.URL, 5*time.Second)
			assert.NoError(t, err)
			assert.NotNil(t, jwks)
			assert.NotNil(t, jwks.Set)

			// Check that all expected kids are present
			for _, kid := range tt.expectedKids {
				// Test enhanced JWKS methods
				jwkKey, err := jwks.GetKeyByID(kid)
				assert.NoError(t, err, "Should be able to get key by ID using enhanced method")
				assert.NotNil(t, jwkKey, "Enhanced key should not be nil")
				assert.Equal(t, kid, jwkKey.KeyID, "Key ID should match")

				// Validate key is properly formed using go-jose
				assert.True(t, jwkKey.Valid(), "Key should be valid according to go-jose")

				// Test key type validation - get the actual key from the JWK
				key := jwkKey.Key
				assert.NotNil(t, key, "Key should not be nil")

				switch {
				case tt.expectedRSA:
					_, isRSA := key.(*rsa.PublicKey)
					assert.True(t, isRSA, "Expected RSA key for %s", kid)
				case tt.expectedEC:
					_, isEC := key.(*ecdsa.PublicKey)
					assert.True(t, isEC, "Expected ECDSA key for %s", kid)
				}
			}

			// Validate that we have keys in the JWKS
			assert.Greater(t, len(jwks.Set.Keys), 0, "Should have keys in JWKS")

			// Test algorithm-specific key retrieval
			for _, expectedAlg := range tt.expectedAlgs {
				keysForAlg := jwks.GetKeysForAlgorithm(expectedAlg)
				assert.NotEmpty(t, keysForAlg, "Should have keys for algorithm %s", expectedAlg)

				// Verify all returned keys are valid
				for _, key := range keysForAlg {
					assert.True(t, key.Valid(), "All keys for algorithm %s should be valid", expectedAlg)
				}
			}
		})
	}
}

func TestNewJWKSFromURL(t *testing.T) {
	type args struct {
		url string
	}

	// Generate a test server so we can capture and inspect the request
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/kas") {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(legacyKasJwks))
		} else if strings.Contains(req.URL.Path, "/ssa") {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(ssaJwks))
		} else if strings.Contains(req.URL.Path, "/keycloak") {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(keycloakJwks))
		} else if strings.Contains(req.URL.Path, "/error") {
			res.WriteHeader(http.StatusInternalServerError)
		} else if strings.Contains(req.URL.Path, "/invalid") {
			res.WriteHeader(http.StatusOK)
			res.Header().Set("Content-Type", "application/json")
			res.Write([]byte(`invalid json`))
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer func() { testServer.Close() }()

	tests := []struct {
		name             string
		args             args
		wantLegacyKasKid *string
		wantSsaKid       *string
		wantKeycloakKid  *string
		wantError        bool
	}{
		{
			name: "fetch and validate legacy KAS JWKS",
			args: args{
				url: testServer.URL + "/kas",
			},
			wantLegacyKasKid: cutil.GetPtr("B3D2:ZQHB:M5YX:NFBJ:DFI4:U4WX:PH5E:4JXH:CNAY:WYTJ:AZWC:RALK"),
		},
		{
			name: "fetch and validate SSA JWKS",
			args: args{
				url: testServer.URL + "/ssa",
			},
			wantSsaKid: cutil.GetPtr("2c58e180-149a-4818-9bfc-5f2a6b6dbd8a"),
		},
		{
			name: "fetch and validate Keycloak JWKS",
			args: args{
				url: testServer.URL + "/keycloak",
			},
			wantKeycloakKid: cutil.GetPtr("2qPROcQfHMCXUi4rKt-CRB5iG4Z-5rfbP7zHOsxWA28"),
		},
		{
			name: "handle server error",
			args: args{
				url: testServer.URL + "/error",
			},
			wantError: true,
		},
		{
			name: "handle invalid JSON",
			args: args{
				url: testServer.URL + "/invalid",
			},
			wantError: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := NewJWKSFromURL(tt.args.url, 5*time.Second)

			if tt.wantError {
				assert.Error(t, err)
				assert.Nil(t, got)
				return
			}

			assert.NoError(t, err)
			assert.NotNil(t, got)
			assert.NotNil(t, got.Set)

			if tt.wantLegacyKasKid != nil {
				// Check that the key exists using GetKeyByID
				jwkKey, err := got.GetKeyByID(*tt.wantLegacyKasKid)
				assert.NoError(t, err)
				assert.NotNil(t, jwkKey)
				// Verify it's an RSA key
				_, isRSA := jwkKey.Key.(*rsa.PublicKey)
				assert.True(t, isRSA)
			}

			if tt.wantSsaKid != nil {
				// Check that the key exists using GetKeyByID
				jwkKey, err := got.GetKeyByID(*tt.wantSsaKid)
				assert.NoError(t, err)
				assert.NotNil(t, jwkKey)
				// Verify it's an ECDSA key
				_, isEC := jwkKey.Key.(*ecdsa.PublicKey)
				assert.True(t, isEC)
			}

			if tt.wantKeycloakKid != nil {
				// Check that the key exists using GetKeyByID
				jwkKey, err := got.GetKeyByID(*tt.wantKeycloakKid)
				assert.NoError(t, err)
				assert.NotNil(t, jwkKey)
				// Verify it's an RSA key
				_, isRSA := jwkKey.Key.(*rsa.PublicKey)
				assert.True(t, isRSA)
			}
		})
	}
}

func TestJWKS_GetKIDPublicKeyMap_EmptyKeys(t *testing.T) {
	emptyJwks := `{"keys":[]}`

	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		res.WriteHeader(http.StatusOK)
		res.Header().Set("Content-Type", "application/json")
		res.Write([]byte(emptyJwks))
	}))
	defer testServer.Close()

	jwks, err := NewJWKSFromURL(testServer.URL, 5*time.Second)
	assert.NoError(t, err)

	// Verify JWKS has no keys
	assert.Empty(t, jwks.Set.Keys, "JWKS should have no keys")
}
