// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"fmt"
	"sync"

	"github.com/rs/zerolog/log"
)

// KeycloakConfig represents the configuration for Keycloak authentication
type KeycloakConfig struct {
	BaseURL               string
	ExternalBaseURL       string
	ClientID              string
	ClientSecret          string
	Realm                 string
	Issuer                string
	ServiceAccountEnabled bool
	jwksConfig            *JwksConfig
	mu                    sync.RWMutex
}

// NewKeycloakConfig creates a new KeycloakConfig instance and attempts to fetch JWKS
func NewKeycloakConfig(baseURL, externalBaseURL, clientID, clientSecret, realm string, serviceAccountEnabled bool) *KeycloakConfig {
	// Construct the issuer URL upfront
	issuer := fmt.Sprintf("%s/realms/%s", externalBaseURL, realm)

	kc := &KeycloakConfig{
		BaseURL:               baseURL,
		ExternalBaseURL:       externalBaseURL,
		ClientID:              clientID,
		ClientSecret:          clientSecret,
		Realm:                 realm,
		Issuer:                issuer,
		ServiceAccountEnabled: serviceAccountEnabled,
	}

	// Initialize and populate JWKS config during construction
	kc.mu.Lock()
	kc.initializeJWKS()
	kc.mu.Unlock()

	return kc
}

// initializeJWKS initializes the JWKS configuration and attempts to fetch keys
// Note: This method should be called with the mutex already locked
// Returns whether the JWKS was successfully fetched
func (kc *KeycloakConfig) initializeJWKS() bool {
	jwksURL := fmt.Sprintf("%s/realms/%s/protocol/openid-connect/certs", kc.BaseURL, kc.Realm)

	jwksConfig := &JwksConfig{
		Name:           fmt.Sprintf("keycloak-%s", kc.Realm),
		URL:            jwksURL,
		Issuer:         kc.Issuer,
		Origin:         TokenOriginKeycloak, // "keycloak"
		ServiceAccount: kc.ServiceAccountEnabled,
	}

	// Retain the configuration even when the initial fetch fails so token
	// validation can refresh the key set after Keycloak becomes available.
	kc.jwksConfig = jwksConfig

	// Attempt to fetch JWKS during initialization
	err := jwksConfig.UpdateJWKS()
	if err != nil {
		log.Warn().Err(err).Msgf("Failed to fetch JWKS during initialization from URL %s for realm %s", jwksURL, kc.Realm)
		return false
	}

	log.Info().Msgf("Successfully initialized JWKS for realm %s from URL %s", kc.Realm, jwksURL)
	return true
}

// GetJwksConfig returns the stable JWKS configuration, initializing it if needed.
func (kc *KeycloakConfig) GetJwksConfig() (*JwksConfig, error) {
	kc.mu.RLock()
	if kc.jwksConfig != nil {
		jwksConfig := kc.jwksConfig
		kc.mu.RUnlock()
		if jwksConfig.GetJWKS() == nil {
			return jwksConfig, fmt.Errorf("failed to fetch JWKS for realm %s", kc.Realm)
		}
		return jwksConfig, nil
	}
	kc.mu.RUnlock()

	kc.mu.Lock()
	defer kc.mu.Unlock()

	// Double-check in case another goroutine initialized it
	if kc.jwksConfig != nil {
		if kc.jwksConfig.GetJWKS() == nil {
			return kc.jwksConfig, fmt.Errorf("failed to fetch JWKS for realm %s", kc.Realm)
		}
		return kc.jwksConfig, nil
	}

	success := kc.initializeJWKS()
	if !success || kc.jwksConfig == nil {
		return kc.jwksConfig, fmt.Errorf("failed to fetch JWKS for realm %s", kc.Realm)
	}

	return kc.jwksConfig, nil
}
