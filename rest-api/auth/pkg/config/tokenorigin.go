// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"context"
	"fmt"
	"sync"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"golang.org/x/sync/singleflight"
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

const (
	// unknownIssuerTTL is how long an issuer the resolver could not find is
	// remembered as absent, so tokens naming a nonexistent issuer cost one query per
	// window rather than one per request.
	unknownIssuerTTL = 5 * time.Second

	// maxUnknownIssuers bounds the unknown-issuer cache. On overflow it is dropped
	// wholesale, since it is only an optimization.
	maxUnknownIssuers = 256

	// DefaultResolveFlightTimeout is the ceiling for one shared resolution when
	// auth.resolveFlightTimeout is unset. It must outlive any single waiter, so the
	// first caller to hang up cannot cancel the lookup the others are waiting on.
	DefaultResolveFlightTimeout = 30 * time.Second
)

// IssuerResolver looks up an issuer that is not in the registry, returning nil
// without an error when no such issuer exists. The api layer installs one backed by
// the issuer table.
//
// A resolver that returns a config must also publish it with AddJwksConfig before
// returning. Publication is the resolver's job because only it can hold the same
// lock the delete path takes, without which a resolution racing a deletion would
// re-add withdrawn trust.
type IssuerResolver func(ctx context.Context, issuerURL string) (*JwksConfig, error)

// TokenOriginConfig holds configuration for token origins with multiple JWKS configs and handlers
type TokenOriginConfig struct {
	sync.RWMutex                           // protects concurrent access to configs and handlers maps
	configs      map[string]*JwksConfig    // map issuer -> JWKSConfig
	processors   map[string]TokenProcessor // map TokenOrigin -> TokenProcessor

	// MetricsNamespace prefixes any metric a processor registers. It is empty when
	// metrics are disabled, which is the signal not to register them at all.
	MetricsNamespace string

	resolver       IssuerResolver
	resolveGroup   singleflight.Group
	unknownIssuers map[string]time.Time
	resolveTimeout time.Duration
}

// NewTokenOriginConfig initializes and returns a configuration object with empty maps
func NewTokenOriginConfig() *TokenOriginConfig {
	return &TokenOriginConfig{
		configs:        make(map[string]*JwksConfig),
		processors:     make(map[string]TokenProcessor),
		resolveTimeout: DefaultResolveFlightTimeout,
	}
}

// SetResolveFlightTimeout sets the ceiling for one on-demand issuer resolution.
// Zero or negative falls back to DefaultResolveFlightTimeout.
func (toc *TokenOriginConfig) SetResolveFlightTimeout(d time.Duration) {
	if d <= 0 {
		d = DefaultResolveFlightTimeout
	}
	toc.Lock()
	toc.resolveTimeout = d
	toc.Unlock()
}

func (toc *TokenOriginConfig) resolveFlightTimeout() time.Duration {
	toc.RLock()
	d := toc.resolveTimeout
	toc.RUnlock()
	if d <= 0 {
		return DefaultResolveFlightTimeout
	}
	return d
}

// SetIssuerResolver installs the resolve-on-miss backend. Passing nil disables
// resolution, which is the default and what Keycloak mode uses.
func (toc *TokenOriginConfig) SetIssuerResolver(r IssuerResolver) {
	toc.Lock()
	defer toc.Unlock()
	toc.resolver = r
}

// ResolveConfig returns the JWKS configuration for an issuer, falling back to the
// installed resolver when the registry does not have it yet, so a replica learns
// about a new issuer from the periodic reload or the first token naming it,
// whichever comes first.
//
// Concurrent misses for the same issuer collapse into one resolver call. It runs on
// its own bounded context so no single waiter's cancellation can abort it, while
// each waiter still returns as soon as its own ctx is done.
func (toc *TokenOriginConfig) ResolveConfig(ctx context.Context, issuer string) *JwksConfig {
	if cfg := toc.GetConfig(issuer); cfg != nil {
		return cfg
	}

	toc.RLock()
	resolver := toc.resolver
	expiry, known := toc.unknownIssuers[issuer]
	toc.RUnlock()

	if resolver == nil || (known && time.Now().Before(expiry)) {
		return nil
	}

	ch := toc.resolveGroup.DoChan(issuer, func() (any, error) {
		resolveCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), toc.resolveFlightTimeout())
		defer cancel()

		// A concurrent reload may have installed it while this call was queued.
		if cfg := toc.GetConfig(issuer); cfg != nil {
			return cfg, nil
		}
		return resolver(resolveCtx, issuer)
	})

	var res singleflight.Result
	select {
	case <-ctx.Done():
		return nil
	case res = <-ch:
	}

	if res.Err != nil {
		// The issuer name is unauthenticated input; without a backoff a store outage
		// turns every request naming an unknown issuer into a DB round-trip.
		log.Warn().Err(res.Err).Str("issuer", issuer).Msg("failed to resolve issuer on demand; backing off")
		toc.rememberUnknownIssuer(issuer)
		return nil
	}

	if cfg, _ := res.Val.(*JwksConfig); cfg == nil {
		toc.rememberUnknownIssuer(issuer)
		return nil
	}

	// The resolver publishes under the lock that orders it against a concurrent
	// withdrawal, so reading the registry back is how this call learns which won: an
	// issuer deleted mid-resolution is absent here and must not be served.
	published := toc.GetConfig(issuer)
	if published == nil {
		toc.rememberUnknownIssuer(issuer)
	}
	return published
}

// rememberUnknownIssuer records an issuer the resolver reported as nonexistent.
func (toc *TokenOriginConfig) rememberUnknownIssuer(issuer string) {
	toc.Lock()
	defer toc.Unlock()

	if toc.unknownIssuers == nil || len(toc.unknownIssuers) >= maxUnknownIssuers {
		toc.unknownIssuers = make(map[string]time.Time, 1)
	}
	toc.unknownIssuers[issuer] = time.Now().Add(unknownIssuerTTL)
}

// AddJwksConfig adds a pre-configured JwksConfig for an issuer
// This is the preferred method for adding configurations
func (toc *TokenOriginConfig) AddJwksConfig(cfg *JwksConfig) {
	toc.Lock()
	defer toc.Unlock()
	toc.configs[cfg.Issuer] = cfg
}

// AddConfig adds a new JWKS config with the specified issuer, URL, origin, and serviceAccount flag
func (toc *TokenOriginConfig) AddConfig(issuer, url string, origin string, serviceAccount bool, audiences []string, scopes []string) {
	toc.Lock()
	defer toc.Unlock()
	toc.configs[issuer] = NewJwksConfig(url, issuer, origin, serviceAccount, audiences, scopes)
}

// AddConfigWithProcessor adds a new JWKS config and processor for the specified origin
func (toc *TokenOriginConfig) AddConfigWithProcessor(issuer, url string, origin string, serviceAccount bool, audiences []string, scopes []string, processor TokenProcessor) {
	toc.Lock()
	defer toc.Unlock()
	toc.configs[issuer] = NewJwksConfig(url, issuer, origin, serviceAccount, audiences, scopes)
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
