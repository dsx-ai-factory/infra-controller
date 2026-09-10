// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"fmt"
	"sync"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
)

// TokenOrigin constants define the source of bearer tokens
// These string values correspond to what's configured in the issuer configmap
const (
	TokenOriginKasLegacy = "kas-legacy" // Legacy KAS tokens
	TokenOriginKasSsa    = "kas-ssa"    // KAS SSA tokens
	TokenOriginKeycloak  = "keycloak"   // Keycloak tokens
	TokenOriginCustom    = "custom"     // Custom/third-party tokens (default if not specified)
	TokenOriginKas       = "kas"        // NGC API keys presented as bearer credentials
)

// AllowedOrigins is the list of valid token origins for the service
var AllowedOrigins = []string{TokenOriginKasLegacy, TokenOriginKasSsa, TokenOriginKeycloak, TokenOriginCustom, TokenOriginKas}

// TokenProcessor interface for processing bearer tokens
type TokenProcessor interface {
	ProcessToken(c echo.Context, tokenStr string, jwksConfig *JwksConfig, logger zerolog.Logger) (*cdbm.User, *util.APIError)
}

// TokenOriginConfig holds configuration for token origins with multiple JWKS configs and handlers
type TokenOriginConfig struct {
	sync.RWMutex                           // protects concurrent access to configs and handlers maps
	configs      map[string]*JwksConfig    // map issuer -> JWKSConfig
	processors   map[string]TokenProcessor // map TokenOrigin -> TokenProcessor

	// MetricsNamespace prefixes any metric a processor registers. It is empty when
	// metrics are disabled, which is the signal not to register them at all.
	MetricsNamespace string
}

// NewTokenOriginConfig initializes and returns a configuration object with empty maps
func NewTokenOriginConfig() *TokenOriginConfig {
	return &TokenOriginConfig{
		configs:    make(map[string]*JwksConfig),
		processors: make(map[string]TokenProcessor),
	}
}

// AddJwksConfig adds a pre-configured JwksConfig for an issuer
// This is the preferred method for adding configurations
func (toc *TokenOriginConfig) AddJwksConfig(cfg *JwksConfig) {
	toc.Lock()
	defer toc.Unlock()
	toc.configs[cfg.Issuer] = cfg
}

// AddConfig adds a new JWKS config with the specified name, issuer, URL, origin, and serviceAccount flag
func (toc *TokenOriginConfig) AddConfig(name, issuer, url string, origin string, serviceAccount bool, audiences []string, scopes []string) {
	toc.Lock()
	defer toc.Unlock()
	toc.configs[issuer] = NewJwksConfig(name, url, issuer, origin, serviceAccount, audiences, scopes)
}

// AddConfigWithProcessor adds a new JWKS config and processor for the specified origin
func (toc *TokenOriginConfig) AddConfigWithProcessor(name, issuer, url string, origin string, serviceAccount bool, audiences []string, scopes []string, processor TokenProcessor) {
	toc.Lock()
	defer toc.Unlock()
	toc.configs[issuer] = NewJwksConfig(name, url, issuer, origin, serviceAccount, audiences, scopes)
	toc.processors[origin] = processor
}

// SetProcessorForOrigin sets a processor for the specified token origin
func (toc *TokenOriginConfig) SetProcessorForOrigin(origin string, processor TokenProcessor) {
	toc.Lock()
	defer toc.Unlock()
	toc.processors[origin] = processor
}

// GetProcessorByOrigin returns the processor for the specified origin
func (toc *TokenOriginConfig) GetProcessorByOrigin(origin string) TokenProcessor {
	toc.RLock()
	defer toc.RUnlock()
	return toc.processors[origin]
}

// GetProcessorByIssuer finds a processor that exactly matches the given issuer
func (toc *TokenOriginConfig) GetProcessorByIssuer(issuer string) TokenProcessor {
	toc.RLock()
	defer toc.RUnlock()
	config := toc.configs[issuer]
	if config != nil {
		return toc.processors[config.Origin]
	}
	return nil
}

// GetConfig returns the JWKS configuration for the specified issuer
func (toc *TokenOriginConfig) GetConfig(issuer string) *JwksConfig {
	toc.RLock()
	defer toc.RUnlock()
	return toc.configs[issuer]
}

// GetConfigsByOrigin returns all JWKS configurations for the specified origin
func (toc *TokenOriginConfig) GetConfigsByOrigin(origin string) map[string]*JwksConfig {
	toc.RLock()
	defer toc.RUnlock()
	result := make(map[string]*JwksConfig)
	for issuer, config := range toc.configs {
		if config.Origin == origin {
			result[issuer] = config
		}
	}
	return result
}

// GetFirstConfigByOrigin returns the first JWKS configuration with the specified origin
func (toc *TokenOriginConfig) GetFirstConfigByOrigin(origin string) *JwksConfig {
	toc.RLock()
	defer toc.RUnlock()
	for _, config := range toc.configs {
		if config.Origin == origin {
			return config
		}
	}
	return nil
}

// RemoveConfig removes the JWKS configuration for the specified issuer
func (toc *TokenOriginConfig) RemoveConfig(issuer string) {
	toc.Lock()
	defer toc.Unlock()
	delete(toc.configs, issuer)
}

// GetAllConfigs returns all JWKS configurations
func (toc *TokenOriginConfig) GetAllConfigs() map[string]*JwksConfig {
	toc.RLock()
	defer toc.RUnlock()
	return toc.configs
}

// UpdateAllJWKS updates the JWKs for all configurations in the map
// Updates are performed in parallel for better performance with multiple issuers.
// Continues on individual failures - only returns error if ALL updates fail.
func (toc *TokenOriginConfig) UpdateAllJWKS() error {
	// Collect configs under lock, then release before network I/O
	toc.RLock()
	jwksConfigs := make([]*JwksConfig, 0, len(toc.configs))
	for _, config := range toc.configs {
		if config != nil && config.URL != "" {
			jwksConfigs = append(jwksConfigs, config)
		}
	}
	toc.RUnlock()

	if len(jwksConfigs) == 0 {
		return nil
	}

	// Update all configs in parallel
	var wg sync.WaitGroup
	errChan := make(chan error, len(jwksConfigs))

	for _, jwksConfig := range jwksConfigs {
		wg.Add(1)
		go func(innerJwksConfig *JwksConfig) {
			defer wg.Done()
			if err := innerJwksConfig.UpdateJWKS(); err != nil {
				log.Warn().Err(err).Str("issuer", innerJwksConfig.Issuer).Msg("Failed to update JWKS")
				errChan <- err
			}
		}(jwksConfig)
	}

	wg.Wait()
	close(errChan)

	// Collect errors and report when all configured issuers were unavailable.
	var errs []error
	for err := range errChan {
		errs = append(errs, err)
	}

	if len(errs) == len(jwksConfigs) {
		return fmt.Errorf("all JWKS updates failed (%d issuers)", len(errs))
	}

	if len(errs) > 0 {
		log.Warn().Int("failed", len(errs)).Int("total", len(jwksConfigs)).Int("succeeded", len(jwksConfigs)-len(errs)).
			Msg("Some JWKS updates failed, continuing with available issuers")
	}

	return nil
}

// GetKeycloakProcessor returns the processor for Keycloak tokens
func (toc *TokenOriginConfig) GetKeycloakProcessor() TokenProcessor {
	toc.RLock()
	defer toc.RUnlock()
	return toc.processors[TokenOriginKeycloak]
}

// GetSsaProcessor returns the processor for SSA tokens
func (toc *TokenOriginConfig) GetSsaProcessor() TokenProcessor {
	toc.RLock()
	defer toc.RUnlock()
	return toc.processors[TokenOriginKasSsa]
}

// GetKasProcessor returns the processor for KAS tokens
func (toc *TokenOriginConfig) GetKasProcessor() TokenProcessor {
	toc.RLock()
	defer toc.RUnlock()
	return toc.processors[TokenOriginKasLegacy]
}

// SetProcessors sets all processors at once for easier initialization
func (toc *TokenOriginConfig) SetProcessors(keycloakProcessor, ssaProcessor, kasProcessor TokenProcessor) {
	toc.Lock()
	defer toc.Unlock()
	toc.processors[TokenOriginKeycloak] = keycloakProcessor
	toc.processors[TokenOriginKasSsa] = ssaProcessor
	toc.processors[TokenOriginKasLegacy] = kasProcessor
}

// IsServiceAccount checks if the given issuer supports service account tokens
func (toc *TokenOriginConfig) IsServiceAccount(issuer string) bool {
	toc.RLock()
	defer toc.RUnlock()
	config := toc.configs[issuer]
	if config != nil {
		return config.ServiceAccount
	}
	return false
}
